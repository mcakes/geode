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
//! The data flow (the source reads, the runs and their answers) is `data`,
//! the cursor keys and the filter `keys`, the body and footer `paint`,
//! the run and publish verbs with the y/n bar `verbs`, the `.` action
//! menu `menu`, and the `:` line's vocabulary `crate::commands`.

use std::collections::BTreeMap;
use std::rc::Rc;

use geode_core::calibration::{Pipeline, StageKind, StageName, StageResult};
use geode_core::config::{Diagnostic, Severity};
use geode_core::link::Group;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_tile::confirm::{self, Confirm};
use geode_tile::following::FollowingQuery;
use geode_tile::header::{Cluster, HEADER_HEIGHT, HealthWatch, MenuTrigger, Mode, link_chips};
use geode_tile::menu::MenuIds;
use geode_tile::motion;
use geode_tile::notice::{self, Dismissals, Notice};
use gpui::prelude::*;
use gpui::{App, Context, ElementId, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::commands::{self, Command};
use crate::content::{ACTIONS, CalibrationConfig, Shared};
use crate::core::layout::Move;
use crate::core::model::GraphModel;
use data::{Fetch, Sources};
use keys::{FilterField, Viewport};
use menu::MenuState;
use paint::{Chrome, Hints};
use verbs::{Pending, Publishing, Uploads};

mod data;
mod keys;
mod menu;
mod paint;
mod verbs;

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
    /// The last verb's refusal (`verbs::refuse`), replaced by the next
    /// and cleared when a verb goes out.
    verb_notice: Option<Notice>,
    /// The uploads' answers since the last publish went out.
    upload_notices: Vec<Notice>,
    /// The armed y/n question: a publish, or (with the editors) a delete.
    confirm: Option<Confirm<Pending>>,
    /// The publish out now, with where its rows go; `None` between.
    publishing: Option<Publishing>,
    /// The tag of the last upload sent; every upload takes the next value.
    upload_tag: u64,
    /// The uploads out, by tag.
    uploads: Uploads,
    /// The `.` action menu while it is up.
    menu: Option<MenuState>,
    /// The menu's element names, formatted once.
    menu_ids: MenuIds,
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
    /// The prepared render model; `None` with no pipeline. Shared with
    /// the edge canvas's paint closure, which clones the handle.
    model: Option<Rc<GraphModel>>,
    model_version: u64,
    /// The element names and caption texts paint reads, rebuilt with the
    /// model.
    chrome: Chrome,
    /// The viewport the layout was last placed in: paint's measurement.
    viewport: Viewport,
    /// The graph's scroll offset, in window pixels; the tile owns it so a
    /// cursor move can bring its card into view.
    scroll: (f32, f32),
    /// The stage filter in force; `None` lights every card.
    filter: Option<String>,
    /// The filter field while `/` has it up: the keys are its.
    filter_field: Option<FilterField>,
    /// The footer's hint rows, resolved against the live keymap.
    hints: Hints,
    /// The `⋯` button's debug selector, formatted once.
    menu_selector: SharedString,
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
        // The footer and the menu name live chords: re-resolved on a
        // keymap reload, never per frame.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.hints = Hints::resolve(cx);
            this.rehint_menu(cx);
            cx.notify();
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
            verb_notice: None,
            upload_notices: Vec::new(),
            confirm: None,
            publishing: None,
            upload_tag: 0,
            uploads: Uploads::new(),
            menu: None,
            menu_ids: menu::menu_ids(id.0),
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
            chrome: Chrome::default(),
            viewport: Viewport::default(),
            scroll: (0.0, 0.0),
            filter: None,
            filter_field: None,
            hints: Hints::resolve(cx),
            menu_selector: format!("calibration-menu-button-{}", id.0).into(),
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

    /// Every notice the header reports, the config's first, then the
    /// reads', the runs', the verbs' and the uploads': what paint filters
    /// through the dismissals and what `escape` dismisses.
    fn all_notices(&self) -> Vec<Notice> {
        self.notices
            .iter()
            .chain(&self.read_notices)
            .chain(&self.run_notice)
            .chain(&self.verb_notice)
            .chain(&self.upload_notices)
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

    /// `insert` while the filter field or an armed question holds the
    /// keys (the shell then routes bare keys to the focused element),
    /// `menu` while the action menu is up, else `normal`.
    fn mode(&self) -> &'static str {
        if self.filter_field.is_some() || self.confirm.is_some() {
            "insert"
        } else if self.menu.is_some() {
            "menu"
        } else {
            "normal"
        }
    }

    /// No `.counts()`: the tile has no counted motion, and the stage
    /// jumps are `g N` chords, so a bare digit is nothing here. With the
    /// menu up, `popup == actions` names it and `tilelist` lets the
    /// shell's shared `j`/`k` and arrows step it.
    pub fn key_context(&self) -> KeyContext {
        let ctx = KeyContext::new(crate::KIND).pair("mode", self.mode());
        if self.menu.is_some() {
            ctx.pair("popup", "actions").tilelist()
        } else {
            ctx
        }
    }

    /// `true` for this module's own registered actions, which the tile
    /// owns whether or not it carries them out yet: a bound key is never
    /// the shell's; and, while the menu is up, for the shell's shared menu
    /// steps. The shell's shared `motion::*` steps are taken as the cursor
    /// keys, so the palette's motions move the cursor too. Anything else
    /// falls through to the shell. A verb other than the menu's own closes
    /// the menu first, and any verb but `cancel` reaching the tile past an
    /// armed question (the palette) answers it no.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let verb = match action.0.as_str() {
            motion::MENU_DOWN if self.menu.is_some() => "list_down",
            motion::MENU_UP if self.menu.is_some() => "list_up",
            id if ACTIONS.iter().any(|(a, _)| *a == id) => &id["calibration::".len()..],
            _ => {
                let Some(m) = motion::parse(action, count) else {
                    return false;
                };
                let step = match m {
                    motion::Motion::Cols(n) if n < 0 => Move::Left,
                    motion::Motion::Cols(_) => Move::Right,
                    motion::Motion::Rows { by, .. } if by < 0 => Move::Up,
                    motion::Motion::Rows { .. } => Move::Down,
                    _ => return false,
                };
                confirm::cancel(self, window, cx);
                self.close_menu(cx);
                self.move_cursor(step, cx);
                return true;
            }
        };
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        if !matches!(
            verb,
            "menu" | "menu_pick" | "cancel" | "list_down" | "list_up"
        ) {
            self.close_menu(cx);
        }
        if verb != "cancel" {
            confirm::cancel(self, window, cx);
        }
        match verb {
            "cursor_left" => self.move_cursor(Move::Left, cx),
            "cursor_right" => self.move_cursor(Move::Right, cx),
            "cursor_up" => self.move_cursor(Move::Up, cx),
            "cursor_down" => self.move_cursor(Move::Down, cx),
            "filter" => self.open_filter(window, cx),
            "commit" => self.commit_filter(window, cx),
            "relayout" => self.relayout(cx),
            "run_from" => self.run_from_cursor(cx),
            "run_all" => {
                if let Err(reason) = self.run_verb(None, cx) {
                    self.refuse(reason, cx);
                }
            }
            "publish" => {
                if let Err(reason) = self.publish_verb(window, cx) {
                    self.refuse(reason, cx);
                }
            }
            "menu" => self.toggle_menu(window, cx),
            "menu_pick" => self.pick_highlighted(window, cx),
            "list_down" => {
                self.step_menu(1, cx);
            }
            "list_up" => {
                self.step_menu(-1, cx);
            }
            // `escape` peels: an armed question, the filter field, the
            // menu; last in line, with nothing else to cancel, it
            // dismisses the header's notices, and is handled only when it
            // did.
            "cancel" => {
                if confirm::cancel(self, window, cx) {
                    return true;
                }
                if self.cancel_filter(window, cx) {
                    return true;
                }
                if self.close_menu(cx) {
                    return true;
                }
                if !self.dismissed.dismiss_all(&self.all_notices()) {
                    return false;
                }
                cx.notify();
            }
            jump if jump.starts_with("jump_") => {
                if let Ok(n) = jump["jump_".len()..].parse::<usize>() {
                    self.jump(n, cx);
                }
            }
            // The edits take their verbs as the editors arrive.
            _ => {}
        }
        true
    }

    /// The `:` line (`crate::commands`): `run [stage]`, `publish`,
    /// `pipeline <name>` and `layout`. A refusal is the line's answer.
    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match commands::parse(line)? {
            Command::Layout => {
                self.relayout(cx);
                Ok(())
            }
            Command::Run(from) => self.run_verb(from, cx),
            Command::Publish => self.publish_verb(window, cx),
            Command::Pipeline(name) => self.switch_pipeline(name, cx),
        }
    }

    /// The verbs, then the shown pipeline's stages after `run` and the
    /// document's pipelines after `pipeline`.
    pub fn completions(&self, line: &str, cursor: usize) -> Vec<String> {
        let config = self.shared.config.borrow();
        let pipelines: Vec<String> = config
            .as_ref()
            .map(|c| {
                c.pipelines
                    .pipelines
                    .iter()
                    .map(|p| p.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let stages: Vec<String> = config
            .as_ref()
            .and_then(|c| self.pipeline_in(c))
            .map(|p| p.stages.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default();
        commands::completions(line, cursor, &stages, &pipelines)
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

    /// The prepared render model, `None` with no pipeline.
    #[cfg(test)]
    pub(crate) fn model(&self) -> Option<&GraphModel> {
        self.model.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// The stage filter in force.
    #[cfg(test)]
    pub(crate) fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn scroll(&self) -> (f32, f32) {
        self.scroll
    }

    /// `(width, height, rem)` the layout was last placed in.
    #[cfg(test)]
    pub(crate) fn viewport(&self) -> (f32, f32, f32) {
        (self.viewport.width, self.viewport.height, self.viewport.rem)
    }

    /// The stages holding a result, by name.
    #[cfg(test)]
    pub(crate) fn result_names(&self) -> Vec<String> {
        self.results.keys().cloned().collect()
    }
}

impl Render for CalibrationTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            selector: self.menu_selector.clone(),
            tip_selector: SharedString::new_static("tip-calibration-menu"),
            action: "calibration::menu",
            open: self.menu.is_some(),
            on_press: {
                let tile = tile.clone();
                Rc::new(move |window, cx| {
                    tile.update(cx, |t, cx| t.toggle_menu(window, cx));
                })
            },
        });
        let header = geode_tile::header::frame(
            self.stack.as_ref().and_then(|s| s.marker(theme, self.id)),
            left,
            cluster,
            theme,
        )
        .debug_selector(move || format!("calibration-header-{id}"));
        // The menu hangs from the header's right edge; the question asks
        // on its own bar under the header, whole at any tile width.
        let menu = self.render_menu(&tile, cx);
        let question = self
            .confirm
            .as_ref()
            .map(|c| confirm::bar(c, &tile, move || format!("calibration-confirm-{id}"), theme));
        let body = if let Some(model) = &self.model {
            self.render_graph(&tile, model, theme, window)
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
        let footer = self.render_footer(self.model.as_deref(), theme);
        let root = v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .debug_selector(move || format!("calibration-{id}"))
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(header)
                    .when_some(menu, |el, m| {
                        el.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(scale::design(HEADER_HEIGHT))
                                .child(m),
                        )
                    }),
            )
            .children(question)
            .child(body)
            .child(footer);
        // A pointer press anywhere on the tile answers an armed question no.
        confirm::cancel_on_press(root, self.confirm.is_some(), &tile)
    }
}

#[cfg(test)]
mod tests;
