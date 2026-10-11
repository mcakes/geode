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
use crate::header;
use geode_core::clock::Clock;
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::grid::selection::SelectKind;
use geode_core::query::{AsOf, QueryKey, parse_as_of};
use geode_core::scenario::{Paint, ShapeSpec};
use geode_core::schema::SchemaSpec;
use geode_core::schema::axis::AxisKind;
use geode_core::scope::{Scope, parse_expr};
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::colfit::{
    FitMetrics, FittedWidths, NOTHING_TO_FIT, SESSION_KEY, widths_from_record, widths_to_toml,
};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::KeyContext;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::tiling::TileId;
use geode_tile::header::HealthWatch;
use geode_tile::notice::{Dismissals, Notice};
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, SharedString, Window};
use gpui_component::ActiveTheme as _;
use gpui_component::table::TableState;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub struct ScenarioTile {
    pub(crate) tile: TileId,
    pub(crate) shape: Arc<ShapeSpec>,
    pub(crate) state: State,
    pub(crate) table: Entity<TableState<ScenarioDelegate>>,
    pub(crate) data: DataHandle,
    pub(crate) frame: FrameRef,
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    pub(crate) health: HealthWatch,
    /// The window's rem the delegate's default widths were laid at.
    last_rem: f32,
    /// The transient notice (a refused key or command); cleared by the next verb.
    pub(crate) refusal: Option<String>,
    /// The one standing notice: a query error, the missing diff base, no data yet.
    pub(crate) standing: Option<Notice>,
    pub(crate) dismissed: Dismissals,
    pub(crate) menu: Option<menu::MenuState>,
    pub(crate) stack: Option<StackHandle>,
    pub(crate) close: Option<CloseHandle>,
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

impl ScenarioTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        shape: Arc<ShapeSpec>,
        restored: Option<&toml::Table>,
        frame: FrameRef,
        data: DataHandle,
        diagnostics: Entity<Diagnostics>,
        schema: Rc<RefCell<SchemaSpec>>,
        dims: Rc<RefCell<DerivedDimensions>>,
        colours: NamedColours,
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
        let mut delegate = ScenarioDelegate::new(&shape.rows, rows_axis_is_scenario, colours);
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
        ScenarioTile {
            tile,
            shape,
            state,
            table,
            data,
            frame,
            schema,
            dims,
            health,
            last_rem: 0.0,
            // A malformed record's first dropped key is the opening notice.
            refusal: notices.into_iter().next(),
            standing: None,
            dismissed: Dismissals::default(),
            menu: None,
            stack: None,
            close: None,
            #[cfg(test)]
            dispatch_log: Vec::new(),
        }
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
                    self.state.currency = match self.state.currency {
                        Currency::Usd => Currency::Native,
                        Currency::Native => Currency::Usd,
                    };
                    self.rebuild_pivot(cx);
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
                    self.state.diff = !self.state.diff;
                    self.rebuild_pivot(cx);
                }
            }
            "escape" => {
                // Peel one layer: the selection, then the refusal, then the
                // reported notices; with none, the key is not handled.
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
        cx.notify();
        true
    }

    /// The notices as the footer reports them: the transient refusal first
    /// (danger, dismissable), else the standing one.
    pub(crate) fn reported_notices(&self) -> Vec<Notice> {
        match (&self.refusal, &self.standing) {
            (Some(r), _) => vec![Notice::danger(r.clone())],
            (None, Some(n)) => vec![n.clone()],
            (None, None) => Vec::new(),
        }
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
        cx.notify();
        Ok(())
    }

    /// The filter must compile against the shape's dataset; an unknown
    /// column is refused here, never dropped or widened (spec §7).
    fn validate_filter(&self, scope: &Scope) -> Result<(), String> {
        let schema = self.schema.borrow();
        let Some(dataset) = schema.dataset(&self.shape.dataset) else {
            return Ok(());
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
        header::render(self, cx)
    }
}
