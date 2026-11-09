//! The shell-hosted calibration tile. It holds the tile's frame handle,
//! the factory's shared config and data handle, answers the shell's door
//! (`crate::content::CalibrationContent`) and paints the tile: the header
//! (the underlying and the pipeline's size, the shared cluster) over a
//! body and a footer the graph, its data flow and its verbs fill in.
//!
//! What paint reads is prepared in `Header` when the tile is built, when
//! the config is pushed and when the followed group changes its
//! underlying, and in the graph model (`core::model`) when the pipeline,
//! the results, the waiting sources or the cursor change, never in render.
//! The data flow (the source reads, the runs and their answers) is `data`.

use std::collections::BTreeMap;
use std::rc::Rc;

use geode_core::calibration::{Pipeline, StageKind, StageName, StageResult};
use geode_core::config::{Diagnostic, Severity};
use geode_core::link::Group;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{CloseHandle, StackHandle, UploadDelivery};
use geode_shell::tiling::TileId;
use geode_tile::following::FollowingQuery;
use geode_tile::header::{Cluster, HealthWatch, MenuTrigger, Mode, link_chips};
use geode_tile::notice::{self, Dismissals, Notice};
use gpui::prelude::*;
use gpui::{App, Context, ElementId, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::content::{ACTIONS, CalibrationConfig, Shared};
use crate::core::model::GraphModel;
use data::{Fetch, Sources};

mod data;

/// The key context of the tile's picker fields, where `tab` completes.
pub const PICKER_CONTEXT: &str = "calibration_picker";

/// The kind's spelling in the header, the palette and the picker.
pub const TITLE: &str = "Calibration";

/// The body's text while the config holds no pipeline of the tile's name.
pub const NO_PIPELINE: &str = "no pipeline";

/// The session keys.
const UNDERLYING_KEY: &str = "underlying";
const PIPELINE_KEY: &str = "pipeline";
const CURSOR_KEY: &str = "cursor";

pub struct CalibrationTile {
    id: TileId,
    frame: FrameRef,
    /// The factory's data handle and the config last pushed.
    shared: Rc<Shared>,
    /// The tile's own underlying, read when it follows no group.
    underlying: Option<String>,
    /// The pipeline shown; `None` means the config's first.
    pipeline_name: Option<String>,
    cursor: Option<StageName>,
    /// Whether the shell last showed the tile; a hidden tile asks nothing
    /// until it is next shown.
    visible: bool,
    stack: Option<StackHandle>,
    /// The shell's close handle; the header paints its × last.
    close: Option<CloseHandle>,
    /// The header's health half: the source stages' datasets.
    health: HealthWatch,
    /// The config's findings, as header notices.
    notices: Vec<Notice>,
    /// The data flow's notices: a failed or refused read, or the
    /// underlying missing. Replaced on each ask.
    read_notices: Vec<Notice>,
    /// The last run's refusal or request failure; cleared by the next run
    /// that goes out or answers.
    run_notice: Option<Notice>,
    /// The source reads under the flip barrier: one tag per sequence.
    following: FollowingQuery<()>,
    fetch: Fetch,
    /// The followed group as of the last frame notification, so a change
    /// that moves no counter is still noticed.
    last_following: Option<Group>,
    sources: Sources,
    /// The underlying the documents and results in hand belong to.
    loaded_for: Option<String>,
    /// Each stage's last result, by name; stages an outcome did not touch
    /// keep theirs.
    results: BTreeMap<StageName, StageResult>,
    /// The tag of the last calibration request of any lane; every request
    /// takes the next value.
    run_tag: u64,
    /// The tag of the last `Run` submitted and of the last user action,
    /// kept apart so a late run is never read as a publish answer.
    last_run_tag: Option<u64>,
    last_user_tag: Option<u64>,
    /// The sources completed while hidden: the run goes out on show.
    run_due: bool,
    /// The prepared render model; `None` with no pipeline.
    model: Option<GraphModel>,
    model_version: u64,
    /// The notices the trader dismissed, each hidden while the config
    /// keeps reporting it; pruned in `refresh`.
    dismissed: Dismissals,
    header: Header,
    /// Every action id the dispatch door received, so a test proves a key
    /// reached the tile through the real keymap rather than calling a verb.
    #[cfg(test)]
    pub(crate) dispatch_log: Vec<ActionId>,
}

/// Prepared paint input, with the inputs it was built from.
#[derive(Default)]
struct Header {
    /// The underlying the header names, as of the last build.
    underlying: Option<String>,
    title: SharedString,
    /// `{n} stages · {k} fits`; `None` with no pipeline.
    counts: Option<SharedString>,
    /// The source stages' datasets, sorted and distinct: the health chip's
    /// question.
    datasets: Vec<String>,
}

/// A config finding as a header notice: an Error is danger, a Warning
/// (a dropped pipeline) warning. The message alone: the layer and file
/// the Display adds belong to the log.
fn notice_of(d: &Diagnostic) -> Notice {
    match d.severity {
        Severity::Error => Notice::danger(d.message.clone()),
        Severity::Warning => Notice::warning(d.message.clone()),
    }
}

impl CalibrationTile {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: TileId,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        shared: Rc<Shared>,
        diagnostics: gpui::Entity<Diagnostics>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CalibrationTile {
        let key = |k: &str| {
            restored
                .and_then(|t| t.get(k))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        };
        // The followed group's underlying is the frame's, and the frame's
        // counters say when the documents must be asked again.
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        cx.observe(&diagnostics, |this, _, cx| {
            let datasets: Vec<&str> = this.header.datasets.iter().map(String::as_str).collect();
            if this
                .health
                .refresh(cx, |d| d.health_for_datasets(&datasets))
            {
                cx.notify();
            }
        })
        .detach();
        let mut tile = CalibrationTile {
            id,
            frame,
            health: HealthWatch::new(diagnostics, id),
            shared,
            underlying: key(UNDERLYING_KEY),
            pipeline_name: key(PIPELINE_KEY),
            cursor: key(CURSOR_KEY),
            visible: false,
            stack: None,
            close: None,
            notices: Vec::new(),
            read_notices: Vec::new(),
            run_notice: None,
            following: FollowingQuery::new(),
            fetch: Fetch::Idle,
            last_following: None,
            sources: Sources::default(),
            loaded_for: None,
            results: BTreeMap::new(),
            run_tag: 0,
            last_run_tag: None,
            last_user_tag: None,
            run_due: false,
            model: None,
            model_version: 0,
            dismissed: Dismissals::default(),
            header: Header::default(),
            #[cfg(test)]
            dispatch_log: Vec::new(),
        };
        tile.last_following = tile.frame.read(cx).following();
        tile.refresh(cx);
        tile
    }

    /// The pipeline the tile shows in `config`: the named one, else the
    /// first. `None` when the config has neither.
    fn pipeline_in<'a>(&self, config: &'a CalibrationConfig) -> Option<&'a Pipeline> {
        match &self.pipeline_name {
            Some(name) => config.pipelines.pipelines.iter().find(|p| p.name == *name),
            None => config.pipelines.pipelines.first(),
        }
    }

    /// The underlying the tile reads, borrowed: the followed group's
    /// single value, else the tile's own.
    fn underlying_str<'a>(&'a self, cx: &'a App) -> Option<&'a str> {
        let frame = self.frame.read(cx);
        match frame.following() {
            Some(_) => geode_core::link::underlying_of(frame.scope()),
            None => self.underlying.as_deref(),
        }
    }

    /// Rebuild what paint reads from the config and the underlying: the
    /// title, the counts, the health question and the notices. Runs when
    /// the tile is built, when a config is pushed and when the followed
    /// group's underlying changes, never in render.
    fn refresh(&mut self, cx: &App) {
        let config = self.shared.config.borrow().clone();
        let underlying = self.underlying_str(cx).map(str::to_string);
        let pipeline = config.as_ref().and_then(|c| self.pipeline_in(c));
        self.header.title = match &underlying {
            Some(u) => format!("{TITLE} \u{b7} {u}").into(),
            None => SharedString::new_static(TITLE),
        };
        self.header.counts = pipeline.map(|p| {
            let fits = p
                .stages
                .iter()
                .filter(|s| matches!(s.kind, StageKind::KalmanFit { .. }))
                .count();
            format!("{} stages \u{b7} {fits} fits", p.stages.len()).into()
        });
        let mut datasets: Vec<String> = pipeline
            .map(|p| {
                p.stages
                    .iter()
                    .filter_map(|s| match &s.kind {
                        StageKind::Source { dataset, .. } => Some(dataset.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        datasets.sort();
        datasets.dedup();
        if datasets != self.header.datasets {
            self.header.datasets = datasets;
            let names: Vec<&str> = self.header.datasets.iter().map(String::as_str).collect();
            self.health.reask(cx, |d| d.health_for_datasets(&names));
        }
        self.header.underlying = underlying;
        self.notices = config
            .as_ref()
            .map(|c| c.diagnostics.iter().map(notice_of).collect())
            .unwrap_or_default();
        // A dismissal lasts while the finding is reported: one the config
        // stopped reporting shows again when it returns.
        let all = self.all_notices();
        self.dismissed.prune(&all);
        self.rebuild_model(cx);
    }

    /// Every notice the header reports, the config's first: what paint
    /// filters through the dismissals and what `escape` dismisses.
    fn all_notices(&self) -> Vec<Notice> {
        self.notices
            .iter()
            .chain(&self.read_notices)
            .chain(&self.run_notice)
            .cloned()
            .collect()
    }

    /// The factory stored a new config: the pipeline is read again by name,
    /// the header follows it, and a shown tile asks its sources again, since
    /// the pipeline's source stages may have changed.
    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
        if self.visible {
            self.requery(cx);
        }
        cx.notify();
    }

    /// `normal` until a field or a list holds the keys.
    fn mode(&self) -> &'static str {
        "normal"
    }

    /// No `.counts()`: the bare digits are stage jumps, and a counting
    /// context would make the matcher swallow them as a pending count.
    pub fn key_context(&self) -> KeyContext {
        KeyContext::new(crate::KIND).pair("mode", self.mode())
    }

    /// `true` for this module's own registered actions, which the tile
    /// owns whether or not it carries them out yet: a bound key is never
    /// the shell's. Anything else falls through to the shell.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = (count, window);
        let verb = match action.0.as_str() {
            id if ACTIONS.iter().any(|(a, _)| *a == id) => &id["calibration::".len()..],
            _ => return false,
        };
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        // Only `cancel` acts so far: last in line, with nothing else to
        // cancel, `escape` dismisses the header's notices, and is handled
        // only when it did. The cursor, the runs, the menu and the edits
        // take their verbs as the graph, the data flow and the editors
        // arrive.
        if verb == "cancel" {
            if !self.dismissed.dismiss_all(&self.all_notices()) {
                return false;
            }
            cx.notify();
        }
        true
    }

    /// The `:` line. No command is read yet; every line is refused.
    pub fn command(
        &mut self,
        line: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Result<(), String> {
        Err(format!("unknown command: {line}"))
    }

    pub fn completions(&self, _line: &str, _cursor: usize) -> Vec<String> {
        Vec::new()
    }

    /// A publish's echo. No publish is asked yet.
    pub fn deliver_upload(&mut self, outcome: UploadDelivery, _cx: &mut Context<Self>) {
        let _ = outcome;
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn set_close(&mut self, close: CloseHandle, cx: &mut Context<Self>) {
        self.close = Some(close);
        cx.notify();
    }

    /// `Calibration · {underlying}`, or `Calibration` with none.
    pub fn title(&self) -> SharedString {
        self.header.title.clone()
    }

    /// `{ underlying, pipeline, cursor }`, each while set. The pipeline is
    /// the one shown: a record saved without one restores to the first
    /// pipeline of the config as it is then, so once the tile knows which
    /// that is it saves the name.
    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        let pipeline = self.pipeline_name.clone().or_else(|| {
            let config = self.shared.config.borrow();
            config
                .as_ref()
                .and_then(|c| self.pipeline_in(c))
                .map(|p| p.name.clone())
        });
        for (key, value) in [
            (UNDERLYING_KEY, &self.underlying),
            (PIPELINE_KEY, &pipeline),
            (CURSOR_KEY, &self.cursor),
        ] {
            if let Some(v) = value {
                t.insert(key.into(), toml::Value::String(v.clone()));
            }
        }
        t
    }

    /// The header's `{n} stages · {k} fits`, `None` with no pipeline.
    #[cfg(test)]
    pub(crate) fn header_counts(&self) -> Option<SharedString> {
        self.header.counts.clone()
    }

    /// The body placeholder's text, `None` while a pipeline is shown.
    #[cfg(test)]
    pub(crate) fn body_text(&self) -> Option<&'static str> {
        self.header.counts.is_none().then_some(NO_PIPELINE)
    }

    /// The header notices that show, in order.
    #[cfg(test)]
    pub(crate) fn notice_texts(&self) -> Vec<String> {
        self.dismissed
            .visible(self.all_notices())
            .iter()
            .map(|n| n.text().to_string())
            .collect()
    }

    /// The prepared render model, `None` with no pipeline. Paint reads it
    /// once the graph is drawn; until then the tests do.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "paint reads the model once the graph is drawn")
    )]
    pub(crate) fn model(&self) -> Option<&GraphModel> {
        self.model.as_ref()
    }

    /// The stages holding a result, by name.
    #[cfg(test)]
    pub(crate) fn result_names(&self) -> Vec<String> {
        self.results.keys().cloned().collect()
    }
}

impl Render for CalibrationTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.0;
        let tile = cx.entity();
        let links = link_chips(&self.frame, cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let left = h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .text_color(theme.foreground)
                    .debug_selector(move || format!("calibration-title-{id}"))
                    .child(self.header.title.clone()),
            )
            .when_some(self.header.counts.clone(), |el, counts| {
                el.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .debug_selector(move || format!("calibration-counts-{id}"))
                        .child(counts),
                )
            });
        let mut cluster = Cluster::new(self.id);
        cluster.mode = Mode::from_key_mode(self.mode());
        cluster.notices = self.dismissed.visible(self.all_notices());
        cluster.on_dismiss = Some(notice::on_dismiss(&tile, |t: &mut CalibrationTile| {
            &mut t.dismissed
        }));
        cluster.links = links;
        cluster.health = self.health.chip();
        cluster.close = self.close.clone();
        cluster.menu = Some(MenuTrigger {
            id: ElementId::NamedInteger(SharedString::new_static("calibration-menu-button"), id),
            selector: format!("calibration-menu-button-{id}").into(),
            tip_selector: SharedString::new_static("tip-calibration-menu"),
            action: "calibration::menu",
            open: false,
            // The trigger is painted so the header's tail has its final
            // shape; it opens nothing until the action menu exists, and
            // is wired to it then.
            on_press: Rc::new(|_, _| {}),
        });
        let header = geode_tile::header::frame(
            self.stack.as_ref().and_then(|s| s.marker(theme, self.id)),
            left,
            cluster,
            theme,
        )
        .debug_selector(move || format!("calibration-header-{id}"));
        let body = if self.header.counts.is_some() {
            // The graph's surface: empty until the layout paints into it.
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .debug_selector(move || format!("calibration-graph-{id}"))
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(
                    div()
                        .debug_selector(move || format!("calibration-empty-{id}"))
                        .child(NO_PIPELINE),
                )
        };
        // The footer row: the key hints and the run's notices take it.
        let footer = h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .text_xs()
            .text_color(muted)
            .border_t_1()
            .border_color(theme.border)
            .debug_selector(move || format!("calibration-footer-{id}"));
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .debug_selector(move || format!("calibration-{id}"))
            .child(header)
            .child(body)
            .child(footer)
    }
}

#[cfg(test)]
mod tests;
