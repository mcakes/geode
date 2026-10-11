//! The scenario tile: one entity per tile, parameterised by its shape. This
//! file holds state, keys, `:` commands and render; `data.rs` the query
//! chain and following; `menu.rs` the `.` menu.

mod data;
mod menu;
#[cfg(test)]
mod tests;

use crate::core::commands::{self, AsOfArg, Command, Vocabulary};
use crate::core::pivot::{Currency, Pivot};
use crate::core::readout::{cursor_readout, tile_title};
use crate::core::session::{self, State, TileAsOf};
use crate::delegate::{CellPointer, ScenarioDelegate};
use crate::header::{self, Chip};
use geode_core::clock::Clock;
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::grid::selection::SelectKind;
use geode_core::link::Group;
use geode_core::query::{AsOf, QueryKey, parse_as_of};
use geode_core::scenario::{Paint, ShapeSpec};
use geode_core::schema::SchemaSpec;
use geode_core::schema::axis::AxisKind;
use geode_core::scope::{Scope, filter_summary, parse_expr};
use geode_core::view::Scale;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::colfit::{
    FitMetrics, FittedWidths, NOTHING_TO_FIT, SESSION_KEY, widths_from_record, widths_to_toml,
};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{FrameRef, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::chip::Tone;
use geode_shell::tiling::TileId;
use geode_tile::following::FollowingQuery;
use geode_tile::header::{DatasetTime, HealthWatch, TimeRun, pinned_chip_text};
use geode_tile::menu::MenuIds;
use geode_tile::notice::{Dismissals, Notice};
use geode_tile::stale::StaleTimer;
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, SharedString, Window};
use gpui_component::ActiveTheme as _;
use gpui_component::table::TableState;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub struct ScenarioTile {
    pub(crate) tile: TileId,
    pub(crate) shape: Arc<ShapeSpec>,
    pub(crate) state: State,
    pub(crate) table: Entity<TableState<ScenarioDelegate>>,
    pub(crate) data: DataHandle,
    pub(crate) frame: FrameRef,
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    /// The named colors, shared with the factory: a reload's colors reach
    /// the paint with the next picture (every applied reload requeries).
    pub(crate) colours: Rc<RefCell<Arc<NamedColours>>>,
    pub(crate) health: HealthWatch,
    /// The window's rem the delegate's default widths were laid at.
    last_rem: f32,
    /// The transient notice (a refused key or command); cleared by the next verb.
    pub(crate) refusal: Option<String>,
    /// A query error or refused submission: it stands until a picture
    /// installs, whatever `c` or `m` rebuild meanwhile (spec §7).
    pub(crate) error: Option<Notice>,
    /// The notice the picture carries: the missing diff base, no data yet,
    /// a snapshot that does not fit. Recomputed with every pivot.
    pub(crate) standing: Option<Notice>,
    pub(crate) dismissed: Dismissals,
    pub(crate) menu: Option<menu::MenuState>,
    /// The `⋯` control's selector and the menu's element names, prepared
    /// once.
    pub(crate) menu_selector: SharedString,
    pub(crate) menu_ids: MenuIds,
    /// The header chips, prepared whenever the state they read changes.
    chips: Vec<Chip>,
    /// The clock and local day a pinned as-of chip was formatted on: a
    /// new day or a zone change re-prepares it.
    chips_day: (Clock, chrono::NaiveDate),
    pub(crate) stack: Option<StackHandle>,
    pub(crate) close: Option<CloseHandle>,
    /// The age past which a dataset time reads stale, shared with the
    /// factory so a reload reaches every tile.
    pub(crate) stale_after: Rc<Cell<Duration>>,
    pub(crate) stale_timer: StaleTimer,
    /// The painted picture's dataset times, labelled on the display clock;
    /// the stale timer arms on their parsed source times.
    pub(crate) times: Vec<DatasetTime>,
    pub(crate) following: FollowingQuery<Arc<data::Fetched>>,
    pub(crate) fetch: data::Fetch,
    pub(crate) domains: data::Domains,
    /// The last chain's answer; `c` and `m` rebuild the pivot over it.
    pub(crate) picture: Option<Arc<data::Fetched>>,
    /// The publication watch on the shape's dataset: the `data` counter.
    pub(crate) watches: Vec<PublicationWatch>,
    pub(crate) last_following: Option<Group>,
    pub(crate) visible: bool,
    #[cfg(test)]
    pub(crate) dispatch_log: Vec<ActionId>,
}

/// The displayed clock: the app's, or the machine's where a tile is hosted
/// without one (tests, a bare window).
pub(crate) fn clock(cx: &App) -> Clock {
    cx.try_global::<geode_shell::clock::AppClock>()
        .map(|c| c.0)
        .unwrap_or_else(|| Clock::machine().0)
}

/// Keep the cursor cell in view after it moved: a motion past the viewport
/// would otherwise leave it, and the footer's readout, off screen.
fn scroll_to_cursor(
    t: &mut TableState<ScenarioDelegate>,
    cx: &mut Context<TableState<ScenarioDelegate>>,
) {
    if let Some((row, col)) = t.delegate().table_cursor() {
        t.scroll_to_row(row, cx);
        t.scroll_to_col(col, cx);
    }
}

impl ScenarioTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        shape: Arc<ShapeSpec>,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        diagnostics: Entity<Diagnostics>,
        stale_after: Rc<Cell<Duration>>,
        schema: Rc<RefCell<SchemaSpec>>,
        dims: Rc<RefCell<DerivedDimensions>>,
        colours: Rc<RefCell<Arc<NamedColours>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (state, notices) = match restored {
            Some(t) => session::from_table(&shape, t),
            None => (State::fresh(&shape), Vec::new()),
        };
        let rows_axis_is_scenario = shape
            .axis(&shape.rows)
            .is_some_and(|a| a.kind == AxisKind::Scenario);
        let mut delegate = ScenarioDelegate::new(
            &shape.rows,
            rows_axis_is_scenario,
            Arc::clone(&colours.borrow()),
        );
        delegate.fitted = widths_from_record(restored);
        delegate.set_paint(state.paint == Paint::Heat, state.scale);
        // The palette is resolved here and on every theme change, never in
        // render.
        delegate.ensure_palette(cx.theme());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(false)
        });
        cx.subscribe(&table, |this, _, e: &CellPointer, cx| this.pointer(*e, cx))
            .detach();
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        // Registered before any read, so the first chain's `data` is the
        // counter every later comparison starts from.
        let watch = frame
            .entity()
            .update(cx, |f, _| f.watch_publications(&shape.dataset, None));
        let last_following = frame.read(cx).following();
        let mut health = HealthWatch::new(diagnostics.clone(), tile);
        let dataset = shape.dataset.clone();
        health.reask(cx, |d| d.health_for_datasets(&[dataset.as_str()]));
        cx.observe(&diagnostics, |this: &mut Self, _, cx| {
            let dataset = this.shape.dataset.clone();
            if this
                .health
                .refresh(cx, |d| d.health_for_datasets(&[dataset.as_str()]))
            {
                cx.notify();
            }
        })
        .detach();
        // The dataset times and a pinned as-of chip read the display clock.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.prepare_times(cx);
            this.prepare_chips(cx);
            cx.notify();
        })
        .detach();
        cx.observe_global::<gpui_component::theme::Theme>(|this, cx| {
            let theme = cx.theme().clone();
            if this
                .table
                .update(cx, |t, _| t.delegate_mut().ensure_palette(&theme))
            {
                cx.notify();
            }
        })
        .detach();
        let (menu_selector, menu_ids) = menu::menu_ids(tile.0);
        let now = chrono::Utc::now();
        let clock = clock(cx);
        let mut this = ScenarioTile {
            tile,
            shape,
            state,
            table,
            data,
            frame,
            schema,
            dims,
            colours,
            health,
            last_rem: 0.0,
            // A malformed record's first dropped key is the opening notice.
            refusal: notices.into_iter().next(),
            error: None,
            standing: None,
            dismissed: Dismissals::default(),
            menu: None,
            menu_selector,
            menu_ids,
            chips: Vec::new(),
            chips_day: (clock, clock.today(now)),
            stack: None,
            close: None,
            stale_after,
            stale_timer: StaleTimer::new(),
            times: Vec::new(),
            following: FollowingQuery::new(),
            fetch: data::Fetch::Idle,
            domains: data::Domains::default(),
            picture: None,
            watches: vec![watch],
            last_following,
            visible: false,
            #[cfg(test)]
            dispatch_log: Vec::new(),
        };
        this.prepare_chips(cx);
        this
    }

    /// The key every read of this tile is submitted and cancelled under.
    pub fn key(&self) -> QueryKey {
        QueryKey(self.tile.0)
    }

    /// `scenario` with the shared grid motions and counts; `mode` is
    /// `menu` while the `.` menu is up, `visual` during a selection (with
    /// `select = rows | block`), else `normal`.
    pub fn key_context(&self, cx: &App) -> KeyContext {
        let selection = self.table.read(cx).delegate().selection.as_ref();
        let mode = if self.menu.is_some() {
            "menu"
        } else if selection.is_some() {
            "visual"
        } else {
            "normal"
        };
        let mut ctx = KeyContext::new(crate::CONTEXT).grid().pair("mode", mode);
        if mode == "menu" {
            ctx = ctx.tilelist();
        }
        if let Some(s) = selection {
            ctx = ctx.pair(
                "select",
                match s.kind {
                    SelectKind::Rows => "rows",
                    SelectKind::Block => "block",
                },
            );
        }
        ctx.counts()
    }

    pub(crate) fn refuse(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.refusal = Some(text.into());
        cx.notify();
    }

    /// One action from the keymap or the palette; `true` when handled.
    /// Every handled verb clears the transient refusal.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(test)]
        self.dispatch_log.push(action.clone());
        if self.menu.is_some()
            && let Some(handled) = self.menu_key(action, window, cx)
        {
            return handled;
        }
        if let Some(m) = geode_tile::motion::parse(action, count) {
            self.refusal = None;
            self.table.update(cx, |t, cx| {
                t.delegate_mut().apply_motion(m);
                scroll_to_cursor(t, cx);
                cx.notify();
            });
            cx.notify();
            return true;
        }
        let Some(name) = action.0.strip_prefix("scenario::") else {
            return false;
        };
        // Read before the verb clears it: `escape` peels it as a layer.
        let refusal = self.refusal.take();
        match name {
            "visual_rows" | "visual_block" => {
                let kind = if name == "visual_rows" {
                    SelectKind::Rows
                } else {
                    SelectKind::Block
                };
                self.table.update(cx, |t, cx| {
                    t.delegate_mut().start_selection(kind);
                    cx.notify();
                });
            }
            "yank" => {
                let text = self.table.update(cx, |t, cx| {
                    let out = t.delegate().yank_text();
                    t.delegate_mut().clear_selection();
                    cx.notify();
                    out
                });
                match text {
                    Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                    None => self.refuse("nothing to yank", cx),
                }
            }
            "currency" => {
                let twinned = self.shape.measures().iter().any(|m| m.usd_twin.is_some());
                if twinned {
                    let previous = self.mode();
                    self.state.currency = match self.state.currency {
                        Currency::Usd => Currency::Native,
                        Currency::Native => Currency::Usd,
                    };
                    self.rebuild_pivot(previous, cx);
                } else {
                    let first = self
                        .shape
                        .measures()
                        .first()
                        .map(|m| m.column.clone())
                        .unwrap_or_default();
                    self.refuse(format!("no USD twin for {first}"), cx);
                }
            }
            "paint" => {
                self.state.paint = match self.state.paint {
                    Paint::Heat => Paint::Plain,
                    Paint::Plain => Paint::Heat,
                };
                self.sync_paint(cx);
            }
            "diff" => {
                if self.shape.diff.is_none() {
                    self.refuse("this shape has no diff", cx);
                } else {
                    let previous = self.mode();
                    self.state.diff = !self.state.diff;
                    self.rebuild_pivot(previous, cx);
                }
            }
            "escape" => {
                // Every verb takes the refusal, so one press clears the
                // selection and a live refusal together; with neither, it
                // dismisses the reported notices; with nothing at all to
                // clear, the key is not handled.
                let had_selection = self.table.update(cx, |t, cx| {
                    let had = t.delegate().selection.is_some();
                    if had {
                        t.delegate_mut().clear_selection();
                        cx.notify();
                    }
                    had
                });
                if !had_selection && refusal.is_none() {
                    let reported = self.reported_notices();
                    if !self.dismissed.dismiss_all(reported.iter()) {
                        return false;
                    }
                }
            }
            "menu" => self.toggle_menu(window, cx),
            // Reached only with no menu up: nothing to pick or close.
            "menu_pick" | "menu_close" => {}
            _ => {
                self.refusal = refusal;
                return false;
            }
        }
        self.state_changed(cx);
        true
    }

    /// After a verb or a `:` line: the chips and an open menu's ticks
    /// follow the state.
    fn state_changed(&mut self, cx: &mut Context<Self>) {
        self.prepare_chips(cx);
        self.refresh_menu(cx);
        cx.notify();
    }

    /// The header chips, as prepared.
    pub(crate) fn chips(&self) -> &[Chip] {
        &self.chips
    }

    /// Prepare the header chips in spec order: currency, scale, `filtered`,
    /// `diff`, `unscoped`, the as-of pin. Every string is formatted here,
    /// never in render.
    pub(crate) fn prepare_chips(&mut self, cx: &App) {
        let id = self.tile.0;
        let chip = |key: &'static str,
                    text: SharedString,
                    tone: Tone,
                    tip: SharedString,
                    action: Option<&'static str>| Chip {
            key,
            text,
            tone,
            tip,
            action,
            selector: format!("tip-{key}-{id}").into(),
        };
        let mut out = Vec::new();
        let native = self.state.currency == Currency::Native;
        let mixed = native && self.currency_mixed(cx);
        out.push(chip(
            "scenario-chip-currency",
            if native { "native" } else { "USD" }.into(),
            if mixed { Tone::Warning } else { Tone::Neutral },
            if mixed {
                "native sums across currencies"
            } else if native {
                "Native currency; `c` for USD"
            } else {
                "USD twins; `c` for native"
            }
            .into(),
            Some("scenario::currency"),
        ));
        if let Some(scale) = self.state.scale {
            out.push(chip(
                "scenario-chip-scale",
                match scale {
                    Scale::Thousands => "k",
                    Scale::Millions => "m",
                    Scale::None => "1",
                }
                .into(),
                Tone::Neutral,
                ":scale 1 restores each column's own".into(),
                None,
            ));
        }
        if !self.state.filter.is_empty() {
            out.push(chip(
                "scenario-chip-filtered",
                "filtered".into(),
                Tone::Neutral,
                format!(
                    "{}; :filter clear removes it",
                    filter_summary(&self.state.filter)
                )
                .into(),
                None,
            ));
        }
        if self.shape.diff.is_some() && self.state.diff {
            out.push(chip(
                "scenario-chip-diff",
                "diff".into(),
                Tone::Neutral,
                "value minus the base row; `m` for absolutes".into(),
                Some("scenario::diff"),
            ));
        }
        if self.state.unscoped {
            out.push(chip(
                "scenario-chip-unscoped",
                "unscoped".into(),
                Tone::Warning,
                "Ignores the shared scope; :unscoped re-attaches it".into(),
                None,
            ));
        }
        let clock = clock(cx);
        let now = chrono::Utc::now();
        match &self.state.as_of {
            TileAsOf::Follow => {}
            TileAsOf::Pinned(AsOf::Live) => out.push(chip(
                "scenario-chip-asof",
                "LIVE".into(),
                Tone::Neutral,
                "Pinned to live; :asof clear follows the frame".into(),
                None,
            )),
            TileAsOf::Pinned(AsOf::At(at)) => out.push(chip(
                "scenario-chip-asof",
                pinned_chip_text(*at, now, clock).into(),
                Tone::Neutral,
                format!(
                    "Pinned to {}; :asof clear follows the frame",
                    clock.full(*at)
                )
                .into(),
                None,
            )),
        }
        self.chips = out;
        self.chips_day = (clock, clock.today(now));
    }

    /// A native sum over rows in more than one currency (spec §5.3): the
    /// currency chip warns.
    pub(crate) fn currency_mixed(&self, cx: &App) -> bool {
        self.table
            .read(cx)
            .delegate()
            .pivot
            .as_ref()
            .is_some_and(|p| p.currency_mixed)
    }

    /// The dataset times as the cluster paints them: stale past
    /// `stale_after`, or once the stale timer fired for that time.
    pub(crate) fn time_runs(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<TimeRun> {
        let after = self.stale_after.get();
        self.times
            .iter()
            .map(|t| TimeRun {
                label: t.label.clone(),
                stale_label: None,
                stale: t.at.is_some_and(|at| {
                    now.signed_duration_since(at).to_std().unwrap_or_default() > after
                        || self.stale_timer.fired_for(at, after)
                }),
            })
            .collect()
    }

    /// The notices as the footer reports them, most urgent first: the
    /// transient refusal (danger, dismissable), the query error, then the
    /// picture's own notice.
    pub(crate) fn reported_notices(&self) -> Vec<Notice> {
        self.refusal
            .iter()
            .map(|r| Notice::danger(r.clone()))
            .chain(self.error.iter().cloned())
            .chain(self.standing.iter().cloned())
            .collect()
    }

    /// A press on a reported notice: the refusal clears, a standing notice
    /// hides until it changes. True when anything changed.
    pub(crate) fn dismiss_notice(&mut self, n: &Notice, cx: &mut Context<Self>) -> bool {
        if !n.dismissable() {
            return false;
        }
        if self.refusal.take().is_some() {
            cx.notify();
            return true;
        }
        let dismissed = self.dismissed.dismiss(n);
        if dismissed {
            cx.notify();
        }
        dismissed
    }

    fn sync_paint(&mut self, cx: &mut Context<Self>) {
        let heat = self.state.paint == Paint::Heat;
        let scale = self.state.scale;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_paint(heat, scale);
            cx.notify();
        });
        cx.notify();
    }

    fn pointer(&mut self, event: CellPointer, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            match event {
                CellPointer::Press { row, col, shift } => d.point(row, col, shift),
                CellPointer::Drag { row, col } => d.point(row, col, true),
                // Focus only: the shell's press route moves it.
                CellPointer::Context { .. } => return,
            }
            scroll_to_cursor(t, cx);
            cx.notify();
        });
        cx.notify();
    }

    /// A tile-local `:` line. Frame-wide verbs are refused by name.
    pub fn command(
        &mut self,
        line: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match commands::parse(line)? {
            Command::Refused(message) => return Err(message.to_string()),
            Command::Unscoped => {
                self.state.unscoped = !self.state.unscoped;
                self.requery(cx);
            }
            Command::FilterExpr(text) => {
                let expr = parse_expr(&text)
                    .map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
                let mut scope = self.state.filter.clone();
                scope.expression = Some(expr);
                self.validate_filter(&scope)?;
                self.state.filter = scope;
                self.requery(cx);
            }
            Command::FilterText(words) => {
                self.state.filter.text = (!words.trim().is_empty()).then_some(words);
                self.requery(cx);
            }
            Command::FilterClear => {
                self.state.filter = Scope::default();
                self.requery(cx);
            }
            Command::AsOf(arg) => {
                let next = match arg {
                    AsOfArg::At(text) => TileAsOf::Pinned(AsOf::At(parse_as_of(
                        &text,
                        chrono::Utc::now(),
                        &clock(cx),
                    )?)),
                    AsOfArg::Live => TileAsOf::Pinned(AsOf::Live),
                    AsOfArg::Clear => TileAsOf::Follow,
                };
                if self.state.as_of != next {
                    self.state.as_of = next;
                    self.requery(cx);
                }
            }
            Command::Scale(scale) => {
                self.state.scale = scale;
                self.sync_paint(cx);
            }
        }
        self.state_changed(cx);
        Ok(())
    }

    /// The filter must compile against the shape's dataset; an unknown
    /// column is refused here, never dropped or widened (spec §7). With no
    /// such dataset in the schema nothing can be checked, so the filter is
    /// refused rather than passed unchecked.
    fn validate_filter(&self, scope: &Scope) -> Result<(), String> {
        let schema = self.schema.borrow();
        let Some(dataset) = schema.dataset(&self.shape.dataset) else {
            return Err(format!(
                "dataset '{}' is not declared: nothing to check the filter against",
                self.shape.dataset
            ));
        };
        let dims = self.dims.borrow();
        match scope.validate(dataset, &dims).into_iter().next() {
            Some(d) => Err(d.message),
            None => Ok(()),
        }
    }

    /// `filter` completes over the dataset's groupable columns (the
    /// grouping compiler's own vocabulary), the derived dimensions and the
    /// shape's measure columns.
    pub fn completions(&self, line: &str, cursor: usize, _cx: &App) -> Vec<String> {
        let schema = self.schema.borrow();
        let mut dimensions: Vec<String> = schema
            .dataset(&self.shape.dataset)
            .map(|ds| {
                ds.groupable_columns()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        dimensions.extend(self.dims.borrow().all().map(|d| d.name.clone()));
        let columns = self.shape.query_measures();
        commands::completions(
            line,
            cursor,
            &Vocabulary {
                dimensions,
                columns,
            },
        )
    }

    /// `{title} · {readout}` over the scope the next query runs under.
    pub fn title(&self, cx: &App) -> SharedString {
        let scope = self.effective_scope_for_title(cx);
        tile_title(&self.shape.title, &scope).into()
    }

    pub fn serialize(&self, cx: &App) -> toml::Table {
        let mut t = session::to_table(&self.shape, &self.state);
        if let Some(w) = widths_to_toml(&self.table.read(cx).delegate().fitted) {
            t.insert(SESSION_KEY.into(), w);
        }
        t
    }

    pub fn autosize_columns(
        &mut self,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let fitted = if reset {
            FittedWidths::new()
        } else {
            let metrics = FitMetrics::xsmall_mono(window.rem_size());
            self.table
                .read(cx)
                .delegate()
                .fit_columns(&metrics)
                .ok_or(NOTHING_TO_FIT)?
        };
        self.table.update(cx, |t, cx| {
            t.delegate_mut().fitted = fitted;
            t.refresh(cx);
        });
        cx.notify();
        Ok(())
    }

    /// `1Y × 100 · 254.5` for the cursor cell; `None` before a picture.
    pub(crate) fn cursor_readout(&self, cx: &App) -> Option<String> {
        let d = self.table.read(cx).delegate();
        let p: &Pivot = d.pivot.as_ref()?;
        let (r, c) = d.cursor;
        Some(cursor_readout(
            p.rows.get(r)?,
            &p.columns.get(c)?.head,
            d.cursor_text(),
        ))
    }
}

impl gpui::Render for ScenarioTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Default column widths follow the window's rem, which the delegate
        // cannot read itself; only a changed rem re-lays the columns.
        let rem = f32::from(window.rem_size());
        if rem != self.last_rem {
            self.last_rem = rem;
            self.table.update(cx, |t, cx| {
                if t.delegate_mut().set_rem(rem) {
                    t.refresh(cx);
                }
            });
        }
        // A pinned as-of chip elides today's date: a new local day (or a
        // zone the clock observer has not yet seen) re-prepares it.
        if matches!(self.state.as_of, TileAsOf::Pinned(AsOf::At(_))) {
            let clock = clock(cx);
            if self.chips_day != (clock, clock.today(chrono::Utc::now())) {
                self.prepare_chips(cx);
            }
        }
        header::render(self, cx)
    }
}
