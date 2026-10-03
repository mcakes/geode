//! The shell-hosted slice viewer entity. It holds the tile's frame handle,
//! its data handle and the catalog it picks underlyings from, answers the
//! shell's door (`crate::content::VolsliceContent`) and paints the tile.
//! The data flow (documents under the flip barrier, the followed group's
//! board, the vol batch and the model swap) is [`data`]'s; the choosers
//! are [`picker`]'s and the chart's pointer gestures [`pointer`]'s.
//!
//! What paint reads beside the model is prepared in [`Chrome`] whenever the
//! tile is notified and an input it was built from changed, never in
//! render: the header's text, the strip's rows, the footer's notice.

mod data;
mod picker;
mod pointer;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::NaiveDate;

use geode_chart::core::palette::Palette;
use geode_chart::core::view::View;
use geode_chart::xy::{XyElement, XyModel};
use geode_core::document::DocumentRows;
use geode_core::link::{DraftMark, Group};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{FrameRef, FrameVersions, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::link::BoardWatch;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_tile::following::FollowingQuery;
use geode_tile::header::{HEADER_HEIGHT, HealthWatch, Mode, link_chips};
use gpui::prelude::*;
use gpui::{
    App, Context, ElementId, Entity, Focusable as _, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, ScrollWheelEvent, SharedString, Window, canvas, div, px,
};
use gpui_component::{ActiveTheme as _, Theme, h_flex, v_flex};

use crate::commands::{self, Command};
use crate::content::ACTIONS;
use crate::core::build::{Plan, with_split};
use crate::core::docs::{CHAIN, CVI};
use crate::core::model::{Kind, Loaded, Pair, State, StripRow};
use crate::core::session;
use crate::header::{self, FooterHint, HeaderModel};
use crate::strip::{self, StripPaint};

use data::{Fetch, Fetched};
use picker::Popup;
use pointer::{ChartBounds, Drag};

pub use picker::PICKER_CONTEXT;

/// One keyboard zoom step: the view narrows (or widens) by this factor.
pub const ZOOM_FACTOR: f64 = 1.25;
/// One keyboard pan step, as a fraction of the view's width.
pub const PAN_STEP: f64 = 0.1;
/// One `[`/`]` step of the split.
pub const SPLIT_STEP: f32 = 0.05;

/// What the tile paints while it reads no underlying.
const EMPTY: &str = "no underlying";
const TITLE: &str = "vol slice";

pub struct VolsliceTile {
    id: TileId,
    frame: FrameRef,
    data: DataHandle,
    /// The underlying picker's catalog.
    diagnostics: Entity<Diagnostics>,
    /// The underlying picker or the diff chooser, while one is up.
    popup: Option<Popup>,
    stack: Option<StackHandle>,
    /// The shell's close handle; the header paints its × last.
    close: Option<CloseHandle>,
    visible: bool,
    state: State,
    /// Both documents under one flip-barrier answer: the pair is handed to
    /// `deliver` once, so the barrier sees one arrival per question.
    following: FollowingQuery<Arc<Fetched>>,
    /// Which of the two document reads is out, under which tag.
    fetch: Fetch,
    /// Data watches on the CVI document and the chain prefix of the
    /// underlying last asked about.
    watches: Vec<PublicationWatch>,
    watched_for: Option<String>,
    /// The followed group's board watch on the CVI document, with the
    /// revision last acted on.
    board: Option<(Group, BoardWatch, u64)>,
    /// The draft the board holds for the underlying it names. It reaches
    /// `loaded` only while that underlying's documents are the ones loaded:
    /// a draft beside another underlying's curves is a plausible wrong
    /// picture.
    board_draft: Option<(String, Arc<DocumentRows>, DraftMark)>,
    /// The group followed as of the last frame notification. Following a
    /// group whose scope was never written moves no version, so this is
    /// compared rather than the versions.
    last_following: Option<Group>,
    loaded: Loaded,
    /// The underlying whose documents `loaded` holds.
    loaded_for: Option<String>,
    /// The versions the documents in `loaded` were asked under, when both
    /// reads answered: `None` after a failed or refused read, whatever the
    /// picture kept, and while nothing is loaded. Only documents read
    /// successfully under the current as-of and publications let a scope
    /// change that keeps the underlying skip the refetch.
    loaded_ok: Option<FrameVersions>,
    /// Moves on every change to `loaded` (documents installed or cleared,
    /// a draft joining or leaving, a mark changing), so a model can be told
    /// apart from the documents now on screen even under one underlying.
    loaded_gen: u64,
    strip: Vec<StripRow>,
    /// The plan of the batch out under `vol_tag`: its answer is read by
    /// position against these roles.
    plan: Option<Plan>,
    /// The `loaded_gen` the painted model was built under; `None` while the
    /// model is empty.
    model_gen: Option<u64>,
    vol_tag: u64,
    model: Arc<XyModel>,
    version: u64,
    /// `None` until the first model, unless restored.
    view: Option<View>,
    full: (f64, f64),
    reset_view: bool,
    palette: Palette,
    /// The theme colors `palette` was derived from.
    palette_key: [Hsla; 7],
    /// Data-side notices: restore, refusals, missing documents, failures.
    notices: Vec<String>,
    /// The painted model's own notices (failed jobs), or a vol refusal.
    model_notices: Vec<String>,
    /// "Today" for tests, whose fixtures are dated: the strip and the chain
    /// drop expiries before today.
    #[cfg(test)]
    pub(crate) today_pin: Option<chrono::NaiveDate>,
    /// Whether the shell last told this tile it is the focused tile. A
    /// strip press acts only when it was; the cursor row is lit only then.
    focused: bool,
    /// The chart surface's last painted bounds (`pointer`).
    chart_bounds: ChartBounds,
    /// The pointer gesture in progress (`pointer`).
    drag: Option<Drag>,
    /// The header's health half: the two datasets' sources.
    health: HealthWatch,
    chrome: Chrome,
    /// Every action id the dispatch door received, so a test proves a key
    /// reached the tile through the real keymap rather than calling a verb.
    #[cfg(test)]
    pub(crate) dispatch_log: Vec<ActionId>,
}

/// What the header names, as of the last header build.
#[derive(Clone, Debug, PartialEq)]
struct HeaderKey {
    underlying: Option<String>,
    coordinate: geode_core::vol::Coordinate,
    kinds: Vec<Kind>,
    mark: Option<DraftMark>,
    hidden: BTreeSet<Kind>,
    diff: Option<Pair>,
}

/// What the strip's rows were built from: the rows, the active set and
/// the palette's theme colors.
type StripKey = (Vec<StripRow>, Option<BTreeSet<NaiveDate>>, [Hsla; 7]);

/// Prepared paint input, each part with the inputs it was built from.
#[derive(Default)]
struct Chrome {
    header: Option<HeaderModel>,
    header_key: Option<HeaderKey>,
    strip: Vec<StripPaint>,
    strip_key: Option<StripKey>,
    notice: Option<geode_tile::notice::Notice>,
    notice_key: Option<(Vec<String>, Vec<String>)>,
    hints: Vec<FooterHint>,
    /// How many parts were rebuilt, for the test that paint formats nothing.
    #[cfg(test)]
    builds: usize,
}

fn palette_key(theme: &Theme) -> [Hsla; 7] {
    [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
        theme.background,
        theme.foreground,
    ]
}

fn palette_of(key: &[Hsla; 7]) -> Palette {
    Palette::from_theme([key[0], key[1], key[2], key[3], key[4]], key[5], key[6])
}

impl VolsliceTile {
    pub fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        diagnostics: Entity<Diagnostics>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> VolsliceTile {
        let (state, notices) = restored.map(session::from_table).unwrap_or_default();
        // A saved view stands until the first model's extent is known; its
        // narrowest span is set then.
        let view = state
            .view
            .map(|(lo, hi)| View::with_min_span((lo, hi), 0.0));
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        // An open picker follows the catalog as it lands; anything else the
        // diagnostics entity announces leaves it alone.
        cx.observe(&diagnostics, |this, _, cx| {
            let health = this
                .health
                .refresh(cx, |d| d.health_for_datasets(&[CVI, CHAIN]));
            if this.refresh_picker(cx) || health {
                cx.notify();
            }
        })
        .detach();
        // The chrome follows every change the tile announces, and only the
        // parts whose inputs moved are rebuilt.
        cx.observe_self(|this, cx| this.refresh_chrome(cx)).detach();
        // The footer names live chords: re-resolved on a keymap reload,
        // never per frame.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chrome.hints = header::footer_hints(cx);
            cx.notify();
        })
        .detach();
        // Expiry colors come from the theme: derived once per theme change,
        // never in render, and the painted model rebuilt with them.
        cx.observe_global::<Theme>(|this, cx| {
            let key = palette_key(cx.theme());
            if key != this.palette_key {
                this.palette_key = key;
                this.palette = palette_of(&key);
                if this.plan.is_some() {
                    this.submit_batch(cx);
                } else {
                    // The strip's dots take the new colors.
                    cx.notify();
                }
            }
        })
        .detach();
        let key = palette_key(cx.theme());
        let mut health = HealthWatch::new(diagnostics.clone(), id);
        health.reask(cx, |d| d.health_for_datasets(&[CVI, CHAIN]));
        let chrome = Chrome {
            hints: header::footer_hints(cx),
            ..Chrome::default()
        };
        let mut tile = VolsliceTile {
            id,
            frame,
            data,
            diagnostics,
            popup: None,
            stack: None,
            close: None,
            visible: false,
            reset_view: view.is_none(),
            view,
            state,
            following: FollowingQuery::new(),
            fetch: Fetch::Idle,
            watches: Vec::new(),
            watched_for: None,
            board: None,
            board_draft: None,
            last_following: None,
            loaded: Loaded::default(),
            loaded_for: None,
            loaded_ok: None,
            loaded_gen: 0,
            strip: Vec::new(),
            plan: None,
            model_gen: None,
            vol_tag: 0,
            model: XyModel::empty(),
            version: 0,
            full: (0.0, 0.0),
            palette: palette_of(&key),
            palette_key: key,
            notices,
            model_notices: Vec::new(),
            focused: false,
            chart_bounds: ChartBounds::default(),
            drag: None,
            health,
            chrome,
            #[cfg(test)]
            today_pin: None,
            #[cfg(test)]
            dispatch_log: Vec::new(),
        };
        tile.refresh_chrome(cx);
        tile
    }

    /// Rebuild each prepared part whose inputs changed since it was built.
    ///
    /// Runs from the tile's self-observer after every notify, and directly
    /// from each door the shell calls inside its draw (`set_visible`),
    /// where the notify is dropped and the observer never runs. The inputs
    /// are compared in place, so a notify that changed nothing (a wheel, a
    /// drag) allocates nothing.
    pub(crate) fn refresh_chrome(&mut self, cx: &App) {
        let underlying = self.painted_underlying(cx);
        let header_stale = self.chrome.header_key.as_ref().is_none_or(|k| {
            k.underlying.as_deref() != underlying
                || k.coordinate != self.state.coordinate
                || !k
                    .kinds
                    .iter()
                    .copied()
                    .eq(Kind::ALL.into_iter().filter(|kind| self.loaded.has(*kind)))
                || k.mark != self.loaded.draft.as_ref().map(|(_, m)| *m)
                || k.hidden != self.state.hidden
                || k.diff != self.state.diff
        });
        if header_stale {
            let key = HeaderKey {
                underlying: underlying.map(str::to_string),
                coordinate: self.state.coordinate,
                kinds: self.loaded.kinds(),
                mark: self.loaded.draft.as_ref().map(|(_, m)| *m),
                hidden: self.state.hidden.clone(),
                diff: self.state.diff,
            };
            self.chrome.header = Some(HeaderModel::prepare(
                key.underlying.as_deref(),
                &self.state,
                &self.loaded,
            ));
            self.chrome.header_key = Some(key);
            #[cfg(test)]
            {
                self.chrome.builds += 1;
            }
        }
        let strip_stale = self
            .chrome
            .strip_key
            .as_ref()
            .is_none_or(|(rows, active, palette)| {
                *rows != self.strip || *active != self.state.active || *palette != self.palette_key
            });
        if strip_stale {
            self.chrome.strip = strip::prepare(&self.strip, &self.state, &self.palette);
            self.chrome.strip_key = Some((
                self.strip.clone(),
                self.state.active.clone(),
                self.palette_key,
            ));
            #[cfg(test)]
            {
                self.chrome.builds += 1;
            }
        }
        let notice_stale = self
            .chrome
            .notice_key
            .as_ref()
            .is_none_or(|(data, model)| *data != self.notices || *model != self.model_notices);
        if notice_stale {
            self.chrome.notice =
                header::footer_notice(self.notices.iter().chain(&self.model_notices));
            self.chrome.notice_key = Some((self.notices.clone(), self.model_notices.clone()));
            #[cfg(test)]
            {
                self.chrome.builds += 1;
            }
        }
    }

    /// The underlying the header names: the one whose documents are on
    /// screen, so a name never stands over another underlying's strip and
    /// curves while a change is in flight or after it failed; with nothing
    /// on screen, the one the tile reads.
    fn painted_underlying<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        self.loaded_for
            .as_deref()
            .or_else(|| self.underlying_str(cx))
    }

    /// The underlying the tile reads, borrowed: the followed group's
    /// single value, else the tile's own. `underlying` is the owned form.
    fn underlying_str<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        let frame = self.frame.read(cx);
        match frame.following() {
            Some(_) => geode_core::link::underlying_of(frame.scope()),
            None => self.state.underlying.as_deref(),
        }
    }

    /// The shell's word on whether this is the focused tile, given from
    /// its render before this tile renders. No notify: one sent there is
    /// dropped. The flag is still painted this frame because the tile's
    /// view is an uncached child of the shell's, re-rendered whenever the
    /// shell renders; a cached tile view would paint the old flag until
    /// its next notify.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The key context's `mode`: `insert` while the picker's field holds
    /// the keys, `menu` while the fieldless diff chooser is up, `normal`
    /// otherwise.
    fn mode(&self) -> &'static str {
        match self.popup {
            Some(Popup::Picker(_)) => "insert",
            Some(Popup::Diff(_)) => "menu",
            None => "normal",
        }
    }

    /// No `.counts()`: the bare digits are kind toggles, and a counting
    /// context would make the matcher swallow them as a pending count.
    /// `tilelist` only under the diff chooser: the picker's field must type
    /// `j` and `k`, which the shared list steps would otherwise claim.
    pub fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(crate::KIND).pair("mode", self.mode());
        if matches!(self.popup, Some(Popup::Diff(_))) {
            ctx.tilelist()
        } else {
            ctx
        }
    }

    /// `true` for this module's own registered actions and, while a list
    /// is up, the shell's shared list steps. Anything else falls through to
    /// the shell. A tile verb other than a popup's own closes the popup
    /// before it runs.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = count;
        let verb = match action.0.as_str() {
            geode_tile::motion::MENU_DOWN if self.popup.is_some() => "list_down",
            geode_tile::motion::MENU_UP if self.popup.is_some() => "list_up",
            id if ACTIONS.iter().any(|(a, _)| *a == id) => &id["volslice::".len()..],
            _ => return false,
        };
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        if !matches!(verb, "commit" | "cancel" | "list_down" | "list_up") {
            self.close_popup(window, cx);
        }
        match verb {
            "strip_down" | "strip_up" => {
                let delta = if verb == "strip_down" { 1 } else { -1 };
                self.state.step_cursor(self.strip.len(), delta);
                cx.notify();
            }
            "solo" => {
                if self.state.solo(&self.strip, self.state.cursor) {
                    self.resubmit(cx);
                }
            }
            "toggle_expiry" => {
                if self.state.toggle(&self.strip, self.state.cursor) {
                    self.resubmit(cx);
                }
            }
            "coordinate" => {
                self.state.cycle_coordinate();
                self.reset_view = true;
                self.resubmit(cx);
            }
            "density" => {
                self.state.density = !self.state.density;
                self.resubmit(cx);
            }
            "diff" => self.open_diff(window, cx),
            "underlying" => match self.frame.read(cx).following() {
                // A follower reads its underlying from the group: picking one
                // here would be overwritten by the next group change.
                Some(g) => {
                    self.notice(following_refusal(g));
                    cx.notify();
                }
                None => self.open_picker(window, cx),
            },
            "pan_left" | "pan_right" | "zoom_in" | "zoom_out" | "reset_view" => {
                self.move_view(verb, cx)
            }
            "split_shrink" => self.step_split(-SPLIT_STEP, cx),
            "split_grow" => self.step_split(SPLIT_STEP, cx),
            "commit" => self.commit_popup(window, cx),
            "cancel" => self.close_popup(window, cx),
            "list_down" => {
                self.step_popup(1, cx);
            }
            "list_up" => {
                self.step_popup(-1, cx);
            }
            kind => {
                // `kind_1`..`kind_9`: the Nth kind in header order. A digit
                // past the kinds does nothing. An unloaded kind's choice is
                // kept, so a draft hidden before it arrives arrives hidden.
                if let Some(n) = kind
                    .strip_prefix("kind_")
                    .and_then(|d| d.parse::<usize>().ok())
                    && let Some(k) = n.checked_sub(1).and_then(|i| Kind::ALL.get(i))
                {
                    self.state.toggle_kind(*k);
                    self.resubmit(cx);
                }
            }
        }
        true
    }

    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match commands::parse(line)? {
            Command::Underlying(u) => {
                if let Some(g) = self.frame.read(cx).following() {
                    return Err(following_refusal(g));
                }
                self.close_popup(window, cx);
                self.set_underlying(u, cx);
            }
            Command::X(c) => {
                if c != self.state.coordinate {
                    self.state.coordinate = c;
                    self.reset_view = true;
                    self.resubmit(cx);
                }
            }
            Command::Diff(pair) => {
                // A pair naming a kind with nothing loaded would ask nothing
                // and paint nothing, silently.
                if let Some(p) = pair {
                    for k in [p.minuend, p.subtrahend] {
                        if !self.loaded.has(k) {
                            return Err(format!("{} is not loaded", k.label()));
                        }
                    }
                }
                if self.state.diff != pair {
                    self.state.diff = pair;
                    self.resubmit(cx);
                }
            }
        }
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        commands::completions(line, cursor, &self.loaded.kinds())
    }

    /// The shell added this tile and it is focused: with no underlying of
    /// its own and no group to read one from, ask for one at once.
    pub fn launched(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.is_none()
            && self.state.underlying.is_none()
            && self.frame.read(cx).following().is_none()
        {
            self.open_picker(window, cx);
        }
    }

    /// The tile's own underlying, from the picker or `:underlying`. The old
    /// question is forgotten, so its late answer is stale; a hidden tile
    /// asks when it is next shown.
    fn set_underlying(&mut self, u: String, cx: &mut Context<Self>) {
        if self.state.underlying.as_deref() == Some(u.as_str()) {
            return;
        }
        self.state.underlying = Some(u);
        self.following.reset();
        self.fetch = Fetch::Idle;
        if self.visible {
            self.requery(cx);
        }
        cx.notify();
    }

    /// Ask again for the batch the state now needs, once documents are
    /// installed; before that there is nothing to ask and the choice waits
    /// for them.
    fn resubmit(&mut self, cx: &mut Context<Self>) {
        if self.loaded_for.is_some() {
            self.submit_batch(cx);
        } else {
            cx.notify();
        }
    }

    /// A keyboard view move, through the axis's own scale so a reversed
    /// axis pans and zooms the way it reads. Zoom anchors at the view's
    /// centre: the host cannot read the element's crosshair. The model is
    /// not rebuilt; the element repaints from its cache key.
    fn move_view(&mut self, verb: &str, cx: &mut Context<Self>) {
        let scale = self.model.x.scale();
        let full = self.full;
        let Some(view) = self.view.as_mut() else {
            return;
        };
        match verb {
            "pan_left" => view.pan(-PAN_STEP * scale.pan_sign(), full),
            "pan_right" => view.pan(PAN_STEP * scale.pan_sign(), full),
            "zoom_in" => view.zoom(ZOOM_FACTOR, scale.about(0.5), full),
            "zoom_out" => view.zoom(1.0 / ZOOM_FACTOR, scale.about(0.5), full),
            _ => view.reset(full),
        }
        self.state.view = Some((view.lo, view.hi));
        cx.notify();
    }

    fn step_split(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.set_split(self.state.split + delta, cx);
    }

    /// Set the split, rounded to a hundredth so the saved value reads as
    /// typed, and clamped as the chart clamps it. The painted model is the
    /// same slots under a new version, which the element's caches need.
    pub(super) fn set_split(&mut self, split: f32, cx: &mut Context<Self>) {
        use geode_chart::core::layout::{SPLIT_MAX, SPLIT_MIN};
        let split = ((split * 100.0).round() / 100.0).clamp(SPLIT_MIN, SPLIT_MAX);
        if split == self.state.split {
            return;
        }
        self.state.split = split;
        self.version += 1;
        self.model = with_split(&self.model, split, self.version);
        cx.notify();
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn set_close(&mut self, close: CloseHandle, cx: &mut Context<Self>) {
        self.close = Some(close);
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        SharedString::new_static(TITLE)
    }

    pub fn serialize(&self) -> toml::Table {
        session::to_table(&self.state)
    }

    /// Whether the picker's own field holds window focus.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        match &self.popup {
            Some(Popup::Picker(p)) => p.input.read(cx).focus_handle(cx).is_focused(window),
            _ => false,
        }
    }

    /// Push a data-side notice once.
    fn notice(&mut self, text: String) {
        if !self.notices.contains(&text) {
            self.notices.push(text);
        }
    }

    /// Every notice in footer order: the data side's, then the model's.
    // Read by the header and footer paint; tests read them now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn notices(&self) -> Vec<String> {
        self.notices
            .iter()
            .chain(&self.model_notices)
            .cloned()
            .collect()
    }

    // Read by the header and footer paint; tests read them now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn model(&self) -> &Arc<XyModel> {
        &self.model
    }

    // Read by the header and footer paint; tests read them now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn view(&self) -> Option<View> {
        self.view
    }

    /// The draft chip's label: `cvi draft`, with the mark's word when it
    /// is not a live edit. `None` while no draft is loaded.
    // Read by the header and footer paint; tests read them now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn draft_label(&self) -> Option<String> {
        let (_, mark) = self.loaded.draft.as_ref()?;
        Some(header::draft_label(*mark))
    }

    #[cfg(test)]
    pub(crate) fn footer_notice(&self) -> Option<SharedString> {
        self.chrome.notice.as_ref().map(|n| n.text().clone())
    }

    #[cfg(test)]
    pub(crate) fn footer_tone(&self) -> Option<geode_tile::notice::Tone> {
        self.chrome.notice.as_ref().map(|n| n.tone())
    }

    /// The underlying the prepared header names.
    #[cfg(test)]
    pub(crate) fn header_underlying(&self) -> SharedString {
        self.chrome
            .header
            .as_ref()
            .map(|h| h.underlying.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn chrome_builds(&self) -> usize {
        self.chrome.builds
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    #[cfg(test)]
    pub(crate) fn empty_text(&self) -> SharedString {
        SharedString::new_static(EMPTY)
    }
}

/// The refusal a follower gives for picking its own underlying.
fn following_refusal(g: Group) -> String {
    format!("following {} \u{2014} set the underlying there", g.letter())
}

impl VolsliceTile {
    /// The chart and its pointer surface: the element, a canvas recording
    /// the bounds the gestures hit-test against, the divider's resize
    /// cursor, and while a drag is armed an occluding catcher that takes
    /// its moves and releases.
    fn render_chart(&self, tile: &Entity<Self>, tile_id: u64, window: &Window) -> impl IntoElement {
        let rem_px = window.rem_size().as_f32();
        let bounds_cell = self.chart_bounds.clone();
        let divider = self.divider_rect(rem_px);
        let drag = self.drag;
        let view = self
            .view
            .unwrap_or_else(|| View::with_min_span(self.model.full(), 0.0));
        div()
            .id(ElementId::NamedInteger(
                SharedString::new_static("volslice-chart-surface"),
                tile_id,
            ))
            .debug_selector(move || format!("volslice-chart-{tile_id}"))
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(XyElement::new(
                self.model.clone(),
                view,
                rem_px,
                // Unique per tile: the element's path caches hang off it.
                ElementId::NamedInteger(SharedString::new_static("volslice-chart"), tile_id),
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
            .when_some(divider, |el, band| {
                el.child(
                    div()
                        .absolute()
                        .left(px(band.x))
                        .top(px(band.y))
                        .w(px(band.w))
                        .h(px(band.h))
                        .cursor_row_resize()
                        .debug_selector(move || format!("volslice-divider-{tile_id}")),
                )
            })
            .when_some(drag, |el, drag| {
                el.child(
                    div()
                        .id(ElementId::NamedInteger(
                            SharedString::new_static("volslice-drag-catcher"),
                            tile_id,
                        ))
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
                            move |_, _window, cx| tile.update(cx, |t, cx| t.drag_finished(cx))
                        })
                        .on_mouse_up_out(MouseButton::Left, {
                            let tile = tile.clone();
                            move |_, _window, cx| tile.update(cx, |t, cx| t.drag_finished(cx))
                        }),
                )
            })
    }
}

impl Render for VolsliceTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        let tile = cx.entity();
        let theme = cx.theme();
        let popup = self
            .popup
            .as_ref()
            .map(|p| picker::render_popup(p, &tile, id, cx));
        let header = self.chrome.header.as_ref().map(|h| {
            header::render_header(
                h,
                theme,
                &tile,
                id,
                self.stack.as_ref(),
                self.close.as_ref(),
                self.health.chip(),
                Mode::from_key_mode(self.mode()),
                link_chips(&self.frame, cx),
            )
        });
        let reads_one = self
            .chrome
            .header_key
            .as_ref()
            .is_some_and(|k| k.underlying.is_some());
        let body = if reads_one || !self.model.slots.is_empty() {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_chart(&tile, id, window))
                .child(strip::render_strip(
                    &self.chrome.strip,
                    self.state.cursor,
                    self.focused,
                    theme,
                    &tile,
                    id,
                ))
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .debug_selector(move || format!("volslice-empty-{id}"))
                        .child(EMPTY),
                )
                .into_any_element()
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .relative()
                    .w_full()
                    .children(header)
                    .when_some(popup, |el, p| {
                        el.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(scale::design(HEADER_HEIGHT))
                                .child(p),
                        )
                    }),
            )
            .child(body)
            .child(header::render_footer(
                self.chrome.notice.as_ref(),
                &self.chrome.hints,
                theme,
                id,
            ))
    }
}

#[cfg(test)]
mod tests;
