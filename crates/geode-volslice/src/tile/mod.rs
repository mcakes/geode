//! The shell-hosted slice viewer entity. It holds the tile's frame handle,
//! its data handle and the catalog it picks underlyings from, answers the
//! shell's door (`crate::content::VolsliceContent`) and paints the tile.
//! The data flow (documents under the flip barrier, the followed group's
//! board, the vol batch and the model swap) is [`data`]'s.

mod data;

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
use gpui::{App, Context, Entity, Hsla, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, v_flex};

use crate::content::ACTIONS;
use crate::core::build::Plan;
use crate::core::model::{Loaded, State, StripRow};
use crate::core::session;

use data::{Fetch, Fetched};

/// What the tile paints while it reads no underlying.
const EMPTY: &str = "no underlying";
const TITLE: &str = "vol slice";

pub struct VolsliceTile {
    id: TileId,
    frame: FrameRef,
    data: DataHandle,
    // The underlying picker's catalog (the picker arrives with the keys).
    #[allow(dead_code)]
    diagnostics: Entity<Diagnostics>,
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

    /// No `.counts()`: the bare digits are kind toggles, and a counting
    /// context would make the matcher swallow them as a pending count.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new(crate::KIND).pair("mode", "normal")
    }

    /// `true` for this module's own registered actions, which the tile
    /// owns whatever state it is in; anything else falls through to the
    /// shell.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = (count, window);
        if !ACTIONS.iter().any(|(id, _)| *id == action.0) {
            return false;
        }
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        if action.0 == "volslice::underlying"
            && let Some(g) = self.frame.read(cx).following()
        {
            // A follower reads its underlying from the group: picking one
            // here would be overwritten by the next group change.
            self.notice(format!(
                "following {} \u{2014} set the underlying there",
                g.letter()
            ));
            cx.notify();
        }
        true
    }

    pub fn command(
        &mut self,
        line: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Result<(), String> {
        Err(format!("unknown command: {}", line.trim()))
    }

    pub fn completions(&self, _line: &str, _cursor: usize) -> Vec<String> {
        Vec::new()
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

    pub fn holds_focus(&self, _window: &Window, _cx: &App) -> bool {
        false
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
    pub(crate) fn empty_text(&self) -> SharedString {
        SharedString::new_static(EMPTY)
    }
}

impl Render for VolsliceTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .debug_selector(move || format!("volslice-empty-{id}"))
                    .child(EMPTY),
            )
    }
}

#[cfg(test)]
mod tests;
