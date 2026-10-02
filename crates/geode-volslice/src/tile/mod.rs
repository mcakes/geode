//! The shell-hosted slice viewer entity. It holds the tile's frame handle,
//! its data handle and the catalog it picks underlyings from, answers the
//! shell's door (`crate::content::VolsliceContent`) and paints the tile.
//! The data flow (documents under the flip barrier, the followed group's
//! board, the vol batch and the model swap) is [`data`]'s.

mod data;
mod picker;

use std::sync::Arc;

use geode_chart::core::palette::Palette;
use geode_chart::core::view::View;
use geode_chart::xy::XyModel;
use geode_core::document::DocumentRows;
use geode_core::link::{DraftMark, Group};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{FrameRef, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::link::BoardWatch;
use geode_shell::module::StackHandle;
use geode_shell::tiling::TileId;
use geode_tile::following::FollowingQuery;
use gpui::prelude::*;
use gpui::{App, Context, Entity, Focusable as _, Hsla, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, v_flex};

use crate::commands::{self, Command};
use crate::content::ACTIONS;
use crate::core::build::{Plan, with_split};
use crate::core::model::{Kind, Loaded, State, StripRow};
use crate::core::session;

use data::{Fetch, Fetched};
use picker::Popup;

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
    strip: Vec<StripRow>,
    /// The plan of the batch out under `vol_tag`: its answer is read by
    /// position against these roles.
    plan: Option<Plan>,
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
    /// Every action id the dispatch door received, so a test proves a key
    /// reached the tile through the real keymap rather than calling a verb.
    #[cfg(test)]
    pub(crate) dispatch_log: Vec<ActionId>,
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
            if this.refresh_picker(cx) {
                cx.notify();
            }
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
                }
            }
        })
        .detach();
        let key = palette_key(cx.theme());
        VolsliceTile {
            id,
            frame,
            data,
            diagnostics,
            popup: None,
            stack: None,
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
            strip: Vec::new(),
            plan: None,
            vol_tag: 0,
            model: XyModel::empty(),
            version: 0,
            full: (0.0, 0.0),
            palette: palette_of(&key),
            palette_key: key,
            notices,
            model_notices: Vec::new(),
            #[cfg(test)]
            today_pin: None,
            #[cfg(test)]
            dispatch_log: Vec::new(),
        }
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
        let kind = crate::core::model::Kind::Draft.label();
        Some(match mark.label() {
            Some(word) => format!("{kind} \u{00b7} {word}"),
            None => kind.to_string(),
        })
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

impl Render for VolsliceTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        let tile = cx.entity();
        let popup = self
            .popup
            .as_ref()
            .map(|p| picker::render_popup(p, &tile, id, cx));
        v_flex()
            .relative()
            .size_full()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .debug_selector(move || format!("volslice-empty-{id}"))
                    .child(EMPTY),
            )
            .when_some(popup, |el, p| {
                el.child(div().absolute().top_0().right_0().child(p))
            })
    }
}

#[cfg(test)]
mod tests;
