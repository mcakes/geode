//! One blotter tile (Phase 3 spec §6.5, §6.7, §6.8): observes the frame,
//! submits keyed queries through `DataHandle`, applies outcomes, records
//! timing, and paints the header strip, the table, and the footer.

use crate::core::commands::{Command, Vocabulary, completions, parse, parse_as_of};
use crate::core::cursor::{Mode, selection};
use crate::core::find::FindState;
use crate::core::flatten::SortSpec;
use crate::core::plan::ColumnKind;
use crate::core::yank::tsv;
use crate::delegate::BlotterDelegate;
use geode_core::groupings::GroupingSlots;
use geode_core::query::{AsOf, QueryKey, QueryOutcome};
use geode_core::scope::{Scope, parse_expr};
use geode_core::view::ViewSpec;
use geode_data::{DataHandle, QueryParams};
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, FindStyle};
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, IntoElement, Window, div, px};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// After this long without a result the header shows an in-flight glyph
/// (foundation §7.1's 50–200 ms affordance).
const IN_FLIGHT_AFTER: Duration = Duration::from_millis(50);

pub const ACTIONS: &[(&str, &str)] = &[
    ("blotter::down", "Cursor down"),
    ("blotter::up", "Cursor up"),
    ("blotter::left", "Cursor left"),
    ("blotter::right", "Cursor right"),
    ("blotter::top", "Cursor to top"),
    ("blotter::bottom", "Cursor to bottom"),
    ("blotter::page_down", "Half page down"),
    ("blotter::page_up", "Half page up"),
    ("blotter::first_col", "First column"),
    ("blotter::last_col", "Last column"),
    ("blotter::expand", "Expand node"),
    ("blotter::collapse", "Collapse node"),
    ("blotter::toggle", "Toggle node"),
    ("blotter::expand_all", "Expand all"),
    ("blotter::collapse_all", "Collapse all"),
    ("blotter::visual", "Visual mode"),
    ("blotter::escape", "Leave visual / clear narrowing"),
    ("blotter::yank", "Yank rows as TSV"),
    ("blotter::find_next", "Next match"),
    ("blotter::find_prev", "Previous match"),
    ("blotter::sort_cycle", "Sort by cursor column"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    None,
    Grouping(Vec<String>),
    Slot(u8),
}

pub struct BlotterTile {
    tile: TileId,
    frame: Entity<Frame>,
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    pub find_style: Rc<Cell<FindStyle>>,
    table: Entity<TableState<BlotterDelegate>>,
    view_name: String,
    pin: Pin,
    unscoped: bool,
    tile_scope: Scope,
    /// The frame versions last acted on; `None` until the first query.
    acted: Option<FrameVersions>,
    tag: u64,
    last_grouping: Vec<String>,
    in_flight: Option<Instant>,
    delivered_at: Option<Instant>,
    visible: bool,
    pub error: Option<String>,
    find: Option<FindState>,
}

impl BlotterTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: Entity<Frame>,
        data: DataHandle,
        views: Rc<RefCell<Vec<ViewSpec>>>,
        find_style: Rc<Cell<FindStyle>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_name = restored
            .and_then(|t| t.get("view").and_then(|v| v.as_str()).map(str::to_string))
            .filter(|n| views.borrow().iter().any(|v| &v.name == n))
            .or_else(|| views.borrow().first().map(|v| v.name.clone()))
            .unwrap_or_default();
        let pin = match restored {
            Some(t) if t.get("pinned_slot").and_then(|v| v.as_integer()).is_some() => {
                Pin::Slot(t["pinned_slot"].as_integer().unwrap() as u8)
            }
            Some(t) if t.get("pinned").and_then(|v| v.as_array()).is_some() => Pin::Grouping(
                t["pinned"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect(),
            ),
            _ => Pin::None,
        };
        let unscoped = restored
            .and_then(|t| t.get("unscoped").and_then(|v| v.as_bool()))
            .unwrap_or(false);

        let table = cx.new(|cx| {
            TableState::new(BlotterDelegate::new(), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .loop_selection(false)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
        });
        cx.subscribe(&table, |this, _, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(row) = event {
                this.table.update(cx, |t, _| {
                    let d = t.delegate_mut();
                    d.cursor.to_row(*row, d.shown.len());
                });
                cx.notify();
            }
        })
        .detach();
        cx.observe(&frame, |this, _, cx| this.on_frame_changed(cx))
            .detach();

        BlotterTile {
            tile,
            frame,
            data,
            views,
            find_style,
            table,
            view_name,
            pin,
            unscoped,
            tile_scope: Scope::default(),
            acted: None,
            tag: 0,
            last_grouping: Vec::new(),
            in_flight: None,
            delivered_at: None,
            visible: false,
            error: None,
            find: None,
        }
    }

    pub fn table(&self) -> &Entity<TableState<BlotterDelegate>> {
        &self.table
    }

    pub fn last_query(&self) -> Option<(u64, Vec<String>)> {
        (self.tag > 0).then(|| (self.tag, self.last_grouping.clone()))
    }

    fn view(&self) -> Option<ViewSpec> {
        self.views
            .borrow()
            .iter()
            .find(|v| v.name == self.view_name)
            .cloned()
    }

    fn grouping(&self, frame: &Frame, view: &ViewSpec) -> Vec<String> {
        match &self.pin {
            Pin::Grouping(g) => g.clone(),
            Pin::Slot(n) => frame
                .slots()
                .get(*n)
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| view.grouping.clone()),
            Pin::None => frame
                .active_grouping()
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| view.grouping.clone()),
        }
    }

    /// Which counters this tile follows (§4.1).
    fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        (!self.unscoped && acted.scope != now.scope)
            || (self.pin == Pin::None && acted.grouping != now.grouping)
            || acted.as_of != now.as_of
            || acted.data != now.data
            || acted.config != now.config
    }

    fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let now = self.frame.read(cx).versions();
        if self.follows_changed(now) {
            self.requery(cx);
        }
        cx.notify();
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.view() else {
            self.error = Some(format!("view '{}' is not configured", self.view_name));
            cx.notify();
            return;
        };
        let (grouping, scope, as_of, versions) = {
            let frame = self.frame.read(cx);
            let grouping = self.grouping(frame, &view);
            let scope = if self.unscoped {
                self.tile_scope.clone()
            } else {
                frame.effective_scope(&self.tile_scope)
            };
            (grouping, scope, frame.as_of().clone(), frame.versions())
        };
        let max_depth = self.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            d.expansion.prune_to(grouping.len());
            d.depth_bound(grouping.len()).max(1)
        });
        self.tag += 1;
        let submitted = Instant::now();
        self.in_flight = Some(submitted);
        self.acted = Some(versions);
        self.last_grouping = grouping.clone();
        let queued = self.data.query(QueryParams {
            key: QueryKey(self.tile.0),
            tag: self.tag,
            submitted,
            view: self.view_name.clone(),
            grouping: Some(grouping),
            scope,
            as_of,
            max_depth,
        });
        if !queued {
            self.error = Some("query refused: the data service is busy or gone".into());
            self.in_flight = None;
        }
        // Repaint once the in-flight affordance is due, if still waiting.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(IN_FLIGHT_AFTER + Duration::from_millis(10))
                .await;
            let _ = this.update(cx, |t, cx| {
                if t.in_flight.is_some() {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.tag {
            return; // stale: a newer request is out
        }
        self.in_flight = None;
        let micros = outcome.submitted.elapsed().as_micros() as u64;
        self.frame
            .update(cx, |f, _| f.requery.record_submit_to_snapshot(micros));
        match outcome.snapshot {
            Ok(snapshot) => {
                self.error = None;
                if let Some(view) = self.view() {
                    let grouping = self.last_grouping.clone();
                    self.table.update(cx, |t, cx| {
                        t.delegate_mut().apply_snapshot(snapshot, &view, &grouping);
                        t.refresh(cx);
                        let row = t.delegate().cursor.row;
                        t.set_selected_row(row, cx);
                    });
                }
                self.delivered_at = Some(Instant::now());
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            let now = self.frame.read(cx).versions();
            if self.follows_changed(now) {
                self.requery(cx);
            }
        }
    }

    pub fn key_context(&self, cx: &App) -> KeyContext {
        let mode = match self.table.read(cx).delegate().mode {
            Mode::Normal => "normal",
            Mode::Visual { .. } => "visual",
        };
        KeyContext::new("blotter").pair("mode", mode).counts()
    }

    fn with_delegate<R>(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut BlotterDelegate) -> R,
    ) -> R {
        self.table.update(cx, |t, _| f(t.delegate_mut()))
    }

    fn sync_cursor(&self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let (row, col) = (t.delegate().cursor.row, t.delegate().cursor.col);
            t.set_selected_row(row, cx);
            t.scroll_to_row(row, cx);
            t.scroll_to_col(col, cx);
        });
    }

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(name) = action.0.strip_prefix("blotter::") else {
            return false;
        };
        let grouping_len = self.last_grouping.len();
        match name {
            "down" | "up" | "top" | "bottom" | "page_down" | "page_up" => {
                let cmd = match name {
                    "down" => NavCommand::Move(1),
                    "up" => NavCommand::Move(-1),
                    "top" => NavCommand::Top,
                    "bottom" => NavCommand::Bottom,
                    "page_down" => NavCommand::Move(5),
                    _ => NavCommand::Move(-5),
                };
                self.with_delegate(cx, |d| {
                    let len = d.shown.len();
                    d.cursor.move_rows(len, cmd, count);
                });
                self.sync_cursor(cx);
            }
            "left" | "right" | "first_col" | "last_col" => {
                self.with_delegate(cx, |d| {
                    let cols = d.plan.as_ref().map_or(0, |p| p.columns.len());
                    match name {
                        "left" => d.cursor.move_cols(cols, -1, count),
                        "right" => d.cursor.move_cols(cols, 1, count),
                        "first_col" => d.cursor.col = 0,
                        _ => d.cursor.col = cols.saturating_sub(1),
                    }
                });
                self.sync_cursor(cx);
            }
            "expand" | "collapse" | "toggle" => {
                let open = match name {
                    "expand" => Some(true),
                    "collapse" => Some(false),
                    _ => None,
                };
                let n = count.unwrap_or(1).max(1);
                let needs_depth = self.with_delegate(cx, |d| {
                    for _ in 0..n {
                        d.expand_cursor(open);
                    }
                    d.cursor_needs_more_depth(grouping_len)
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
                if needs_depth {
                    self.requery(cx);
                }
            }
            "expand_all" | "collapse_all" => {
                // `expand_all` always requeries with the full depth
                // (`depth_bound` returns `grouping_len` under
                // `open_all`); `collapse_all` never does.
                let needs_depth = name == "expand_all";
                self.with_delegate(cx, |d| {
                    if name == "expand_all" {
                        d.expansion.open_all();
                    } else {
                        d.expansion.close_all();
                    }
                    d.reflatten();
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
                if needs_depth {
                    self.requery(cx);
                }
            }
            "visual" => {
                self.with_delegate(cx, |d| {
                    d.mode = match d.mode {
                        Mode::Normal => Mode::Visual {
                            anchor: d.cursor.row,
                        },
                        Mode::Visual { .. } => Mode::Normal,
                    };
                });
            }
            "escape" => {
                self.with_delegate(cx, |d| {
                    d.mode = Mode::Normal;
                    if d.narrowed.is_some() {
                        d.set_narrowed(None);
                    }
                });
                self.find = None;
                self.table.update(cx, |t, cx| t.refresh(cx));
            }
            "yank" => {
                let text = self.with_delegate(cx, |d| {
                    let (Some(snapshot), Some(plan)) = (&d.snapshot, &d.plan) else {
                        return None;
                    };
                    let range = selection(&d.mode, &d.cursor);
                    let out = tsv(snapshot, plan, &d.shown, range);
                    d.mode = Mode::Normal;
                    Some(out)
                });
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "find_next" | "find_prev" => {
                let dir = if name == "find_next" {
                    FindDirection::Forward
                } else {
                    FindDirection::Backward
                };
                if let Some(find) = &self.find {
                    let (texts, from) = self.with_delegate(cx, |d| (d.shown_texts(), d.cursor.row));
                    if let Some(row) = find.repeat(&texts, from, dir, count) {
                        self.with_delegate(cx, |d| {
                            let len = d.shown.len();
                            d.cursor.to_row(row, len);
                        });
                        self.sync_cursor(cx);
                    }
                }
            }
            "sort_cycle" => {
                self.with_delegate(cx, |d| {
                    let col = d.cursor.col;
                    if col == 0 {
                        return;
                    }
                    d.sort = match d.sort {
                        Some(s) if s.column == col && !s.descending => Some(SortSpec {
                            column: col,
                            descending: true,
                        }),
                        Some(s) if s.column == col => None,
                        _ => Some(SortSpec {
                            column: col,
                            descending: false,
                        }),
                    };
                    d.reflatten();
                });
                self.table.update(cx, |t, cx| {
                    t.refresh_header_layout(cx);
                    t.refresh(cx);
                });
                self.sync_cursor(cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match parse(line)? {
            Command::Group(g) => {
                self.pin = Pin::Grouping(g);
                self.requery(cx);
            }
            Command::GroupSlot(n) => {
                if self.frame.read(cx).slots().get(n).is_none() {
                    return Err(format!("slot {n} is empty"));
                }
                self.pin = Pin::Slot(n);
                self.requery(cx);
            }
            Command::GroupSave(n) => {
                let grouping = self.last_grouping.clone();
                if grouping.is_empty() {
                    return Err("nothing grouped yet".into());
                }
                let result = self.frame.update(cx, |f, cx| {
                    let r = f.save_slot(n, grouping);
                    cx.notify();
                    r
                });
                result?;
            }
            Command::Unpin => {
                self.pin = Pin::None;
                self.requery(cx);
            }
            Command::Unscoped => {
                self.unscoped = !self.unscoped;
                self.requery(cx);
            }
            Command::ScopeExpr(text) => {
                let expr = parse_expr(&text)
                    .map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
                self.frame.update(cx, |f, cx| {
                    let mut scope = f.scope().clone();
                    scope.expression = Some(expr);
                    if f.set_scope(scope) {
                        cx.notify();
                    }
                });
            }
            Command::ScopeText(words) => {
                self.frame.update(cx, |f, cx| {
                    let mut scope = f.scope().clone();
                    scope.text = (!words.is_empty()).then_some(words);
                    if f.set_scope(scope) {
                        cx.notify();
                    }
                });
            }
            Command::ScopeClear => {
                self.frame.update(cx, |f, cx| {
                    if f.clear_scope() {
                        cx.notify();
                    }
                });
            }
            Command::ScopeUndo => {
                let undone = self.frame.update(cx, |f, cx| {
                    let r = f.undo_scope();
                    cx.notify();
                    r
                });
                if !undone {
                    return Err("nothing to undo".into());
                }
            }
            Command::AsOf(text) => {
                let at = parse_as_of(&text, chrono::Utc::now())?;
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::At(at)) {
                        cx.notify();
                    }
                });
            }
            Command::Live => {
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::Live) {
                        cx.notify();
                    }
                });
            }
            Command::View(name) => {
                if !self.views.borrow().iter().any(|v| v.name == name) {
                    return Err(format!("no view named '{name}'"));
                }
                self.view_name = name;
                self.with_delegate(cx, |d| d.plan = None);
                self.requery(cx);
            }
            Command::Sort { column, descending } => {
                let found = self.with_delegate(cx, |d| {
                    let col = d
                        .plan
                        .as_ref()?
                        .columns
                        .iter()
                        .position(|c| c.name == column)?;
                    d.sort = Some(SortSpec {
                        column: col,
                        descending,
                    });
                    d.reflatten();
                    Some(())
                });
                if found.is_none() {
                    return Err(format!("no column named '{column}' in this view"));
                }
                self.table.update(cx, |t, cx| {
                    t.refresh_header_layout(cx);
                    t.refresh(cx);
                });
            }
            Command::SortClear => {
                self.with_delegate(cx, |d| {
                    d.sort = None;
                    d.reflatten();
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
            }
        }
        cx.notify();
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        // Before the first snapshot lands, `plan` is `None` — fall back to
        // the view's own declared columns so `:sort`/`:group` completion
        // works from the moment a tile opens, not only after a delivery.
        let columns = self
            .table
            .read(cx)
            .delegate()
            .plan
            .as_ref()
            .map(|p| {
                p.columns
                    .iter()
                    .filter(|c| c.kind != ColumnKind::Tree)
                    .map(|c| c.name.clone())
                    .collect()
            })
            .or_else(|| {
                self.view()
                    .map(|v| v.columns.iter().map(|c| c.name().to_string()).collect())
            })
            .unwrap_or_default();
        let views = self.views.borrow().iter().map(|v| v.name.clone()).collect();
        completions(line, cursor, &Vocabulary { columns, views })
    }

    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed(query) => {
                if self.find.is_none() {
                    let origin = self.table.read(cx).delegate().cursor.row;
                    self.find = Some(FindState::begin(self.find_style.get(), origin));
                }
                let style = self.find_style.get();
                let texts = self.table.read(cx).delegate().shown_texts();
                let find = self.find.as_mut().unwrap();
                let hit = find.changed(&texts, &query);
                let narrowed = find.narrowed.clone();
                self.with_delegate(cx, |d| {
                    if style == FindStyle::Fzf {
                        d.set_narrowed(narrowed);
                    }
                    if let Some(row) = hit {
                        let len = d.shown.len();
                        d.cursor.to_row(row, len);
                    }
                });
                self.table.update(cx, |t, cx| t.refresh(cx));
                self.sync_cursor(cx);
            }
            FindEvent::Committed(query) => {
                if let Some(find) = self.find.as_mut() {
                    find.committed(&query);
                }
            }
            FindEvent::Cancelled => {
                if let Some(mut find) = self.find.take() {
                    let origin = find.cancelled();
                    self.with_delegate(cx, |d| {
                        d.set_narrowed(None);
                        let len = d.shown.len();
                        d.cursor.to_row(origin, len);
                    });
                    self.table.update(cx, |t, cx| t.refresh(cx));
                    self.sync_cursor(cx);
                }
            }
        }
        cx.notify();
    }

    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("view".into(), toml::Value::String(self.view_name.clone()));
        match &self.pin {
            Pin::None => {}
            Pin::Grouping(g) => {
                t.insert(
                    "pinned".into(),
                    toml::Value::Array(g.iter().map(|s| toml::Value::String(s.clone())).collect()),
                );
            }
            Pin::Slot(n) => {
                t.insert("pinned_slot".into(), toml::Value::Integer(*n as i64));
            }
        }
        t.insert("unscoped".into(), toml::Value::Boolean(self.unscoped));
        t
    }
}

impl gpui::Render for BlotterTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The paint half of §7.1 (§6.8): the first render after a delivery.
        if let Some(at) = self.delivered_at.take() {
            let micros = at.elapsed().as_micros() as u64;
            self.frame
                .update(cx, |f, _| f.requery.record_snapshot_to_paint(micros));
        }
        let theme = cx.theme();
        let frame = self.frame.read(cx);
        let stale_after = Duration::from_secs(15 * 60);
        let delegate = self.table.read(cx).delegate();
        let snapshot = delegate.snapshot.clone();

        // Header strip: view · grouping · markers · freshness · AS OF · … · error
        let mut header = h_flex()
            .w_full()
            .h(px(22.))
            .items_center()
            .gap_3()
            .px_2()
            .text_sm()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .debug_selector(|| format!("blotter-header-{}", self.tile.0))
            .child(
                div()
                    .text_color(theme.foreground)
                    .child(self.view_name.clone()),
            )
            .child(div().child(GroupingSlots::label_of(&self.last_grouping)));
        match &self.pin {
            Pin::None => {}
            _ => {
                header = header.child(
                    div()
                        .text_color(theme.warning_foreground)
                        .bg(theme.warning.opacity(0.25))
                        .px_1()
                        .rounded(px(3.))
                        .child("pinned"),
                )
            }
        }
        if self.unscoped {
            header = header.child(
                div()
                    .text_color(theme.warning_foreground)
                    .bg(theme.warning.opacity(0.25))
                    .px_1()
                    .rounded(px(3.))
                    .child("unscoped"),
            );
        }
        if let Some(snapshot) = &snapshot {
            let p = snapshot.provenance();
            let mut datasets: Vec<_> = p.datasets.iter().collect();
            datasets.sort_by(|a, b| a.as_of.cmp(&b.as_of));
            let now = chrono::Utc::now();
            for f in datasets {
                let text = match &f.as_of {
                    Some(t) => format!("{} {}", f.dataset, &t[11..16.min(t.len())]),
                    None => format!("{} —", f.dataset),
                };
                let stale = f
                    .as_of
                    .as_deref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| {
                        now.signed_duration_since(t.with_timezone(&chrono::Utc))
                            .to_std()
                            .unwrap_or_default()
                            > stale_after
                    });
                header = header.child(
                    div()
                        .when(stale, |el| el.text_color(theme.warning))
                        .child(text),
                );
            }
            if let Some(req) = &p.as_of_request {
                header = header.child(
                    div()
                        .text_color(theme.warning_foreground)
                        .bg(theme.warning.opacity(0.4))
                        .px_1()
                        .rounded(px(3.))
                        .child(format!("AS OF {}", &req[..16.min(req.len())])),
                );
            }
        }
        if self
            .in_flight
            .is_some_and(|t| t.elapsed() > IN_FLIGHT_AFTER)
        {
            header = header.child(div().child("…"));
        }
        if let Some(e) = &self.error {
            header = header.child(div().text_color(theme.danger).child(e.clone()));
        }

        // Footer: counts and legends.
        let mut footer = h_flex()
            .w_full()
            .h(px(20.))
            .items_center()
            .gap_4()
            .px_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .child(div().child(format!("{} rows", delegate.shown.len())));
        if delegate.any_determined {
            footer = footer.child(div().child("† shown for this row, do not total"));
        }
        if !delegate.semi_joined.is_empty() {
            footer = footer.child(div().child(format!(
                "⋈ scoped by membership on {}: whole entities that qualify, not their share",
                delegate.semi_joined.join(", ")
            )));
        }
        if delegate.unplaced > 0 {
            footer = footer.child(
                div()
                    .text_color(theme.warning)
                    .child(format!("{} rows unplaced", delegate.unplaced)),
            );
        }
        let _ = frame;

        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.tile.0))
            .child(header)
            .child(
                div().flex_1().min_h_0().w_full().child(
                    DataTable::new(&self.table)
                        .with_size(Size::XSmall)
                        .bordered(false)
                        .stripe(false),
                ),
            )
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::groupings::GroupingSlots;
    use geode_core::query::{QueryKey, QueryOutcome};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::frame::Frame;
    use geode_shell::module::FindEvent;
    use geode_shell::tiling::TileId;
    use geode_shell::vimfind::FindStyle;
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    fn views() -> Vec<ViewSpec> {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[tree.columns]]\nname = \"delta01\"\n[[tree.columns]]\nname = \"daily_trading_pnl\"\n[wide]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[wide.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0
    }

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["lhu".into()]);
        s.set(2, vec!["underlying_ref".into(), "lhu".into()]);
        s
    }

    /// Root; L1, L2; L1/SPX. Trading PnL is NonAttributable at depth 2.
    fn snapshot() -> Arc<Snapshot> {
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta {
            name: n.into(),
            attribution_by_depth: by_depth,
            scope_semantics: ScopeSemantics::Direct,
        };
        Arc::new(Snapshot::for_tests(
            vec![
                (
                    meta("lhu", vec![Attribution::Additive; 3]),
                    TestColumn::Dict(vec![
                        None,
                        Some("L1".into()),
                        Some("L2".into()),
                        Some("L1".into()),
                    ]),
                ),
                (
                    meta("underlying_ref", vec![Attribution::Additive; 3]),
                    TestColumn::Dict(vec![None, None, None, Some("SPX".into())]),
                ),
                (
                    meta("row_depth", vec![Attribution::Additive; 3]),
                    TestColumn::I32(vec![0, 1, 1, 2]),
                ),
                (
                    meta("delta01", vec![Attribution::Additive; 3]),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0), Some(5.0)]),
                ),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![
                            Attribution::Additive,
                            Attribution::Additive,
                            Attribution::NonAttributable,
                        ],
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0), Some(7.0), None]),
                ),
            ],
            2,
        ))
    }

    struct Harness {
        tile: Entity<BlotterTile>,
        frame: Entity<Frame>,
        requests: Receiver<Request>,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (data, requests) = DataHandle::for_tests();
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(slots(), None));
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
                                data.clone(),
                                Rc::new(RefCell::new(views())),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                None,
                                window,
                                cx,
                            )
                        });
                        Host { tile, frame }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (tile, frame) = window
            .root(&mut vcx)
            .unwrap()
            .read_with(&vcx, |h, _| (h.tile.clone(), h.frame.clone()));
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile,
                frame,
                requests,
            },
            vcx,
        )
    }

    /// A root view for the test window that just paints the tile.
    struct Host {
        tile: Entity<BlotterTile>,
        frame: Entity<Frame>,
    }
    impl gpui::Render for Host {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.tile.clone())
        }
    }

    fn next_query(rx: &Receiver<Request>) -> geode_data::QueryParams {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("a request") {
                Request::Query(p) => return p,
                _ => continue,
            }
        }
    }

    fn deliver(
        h: &Harness,
        cx: &mut gpui::VisualTestContext,
        tag: u64,
        snapshot: Result<Arc<Snapshot>, String>,
    ) {
        h.tile.update(cx, |t, cx| {
            t.deliver(
                QueryOutcome {
                    key: QueryKey(7),
                    tag,
                    snapshot,
                    submitted: Instant::now() - Duration::from_millis(12),
                },
                cx,
            )
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn showing_the_tile_submits_one_query_keyed_by_the_tile_with_the_views_grouping(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        assert_eq!(p.key, QueryKey(7));
        assert_eq!(p.view, "tree");
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["lhu".to_string(), "underlying_ref".into()][..])
        );
        assert_eq!(p.max_depth, 1, "collapsed: one level");
        assert!(h.requests.try_recv().is_err(), "exactly one");
    }

    #[gpui::test]
    fn a_frame_slot_change_requeries_once_and_a_pinned_tile_ignores_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.frame.update(&mut cx, |f, cx| {
            f.set_active_slot(Some(2));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["underlying_ref".to_string(), "lhu".into()][..])
        );
        assert!(h.requests.try_recv().is_err());

        h.tile
            .update(&mut cx, |t, cx| t.command("group lhu", cx).unwrap());
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["lhu".to_string()][..]),
            "pinned"
        );
        h.frame.update(&mut cx, |f, cx| {
            f.set_active_slot(Some(1));
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a pinned tile does not follow the slot"
        );
        h.tile
            .update(&mut cx, |t, cx| t.command("unpin", cx).unwrap());
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["lhu".to_string()][..]),
            "rejoined slot 1"
        );
    }

    #[gpui::test]
    fn a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let rows = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(rows, vec![0, 1, 2]);
        assert!(
            h.frame.read_with(&cx, |f, _| f.requery.last()).is_some(),
            "submit→snapshot and snapshot→paint recorded"
        );

        // `Snapshot::for_tests` cannot build a truly empty (zero-column)
        // snapshot — `RecordBatch::try_new` refuses it. A one-row
        // one-column snapshot exercises the same "stale tag" path just as
        // well: the assertion is that it never reaches `shown` at all.
        let meta1 = ColumnMeta {
            name: "lhu".into(),
            attribution_by_depth: vec![Attribution::Additive],
            scope_semantics: ScopeSemantics::Direct,
        };
        let stale_snapshot = Arc::new(Snapshot::for_tests(
            vec![(meta1, TestColumn::Dict(vec![Some("X".into())]))],
            1,
        ));
        deliver(&h, &mut cx, p.tag + 100, Ok(stale_snapshot));
        let rows = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(rows, vec![0, 1, 2], "a stale tag changed nothing");

        h.tile
            .update(&mut cx, |t, cx| t.command("view wide", cx).unwrap());
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Err("binder error".into()));
        let (rows, error) = h.tile.read_with(&cx, |t, cx| {
            (t.table().read(cx).delegate().shown.clone(), t.error.clone())
        });
        assert_eq!(rows, vec![0, 1, 2], "the last good snapshot stays");
        assert_eq!(error.as_deref(), Some("binder error"));
    }

    #[gpui::test]
    fn motions_expansion_and_yank(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let act = |cx: &mut gpui::VisualTestContext, id: &str, count: Option<u32>| {
            h.tile
                .update(cx, |t, cx| t.dispatch(&ActionId(id.into()), count, cx))
        };
        assert!(act(&mut cx, "blotter::down", Some(2)));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row),
            2
        );
        act(&mut cx, "blotter::up", None);
        act(&mut cx, "blotter::expand", None);
        let rows = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(
            rows,
            vec![0, 1, 3, 2],
            "L1 opened; SPX is already materialised"
        );
        assert!(
            h.requests.try_recv().is_err(),
            "no requery: the child was in hand"
        );

        // DFS order puts SPX (L1's already-materialised child) right
        // after L1 in `shown`; the row that genuinely lacks a fetched
        // child at this point is L2, two rows down from L1.
        act(&mut cx, "blotter::down", Some(2));
        act(&mut cx, "blotter::expand", None);
        let p = next_query(&h.requests);
        assert_eq!(
            p.max_depth, 2,
            "opening at the bound requeries one level deeper"
        );

        act(&mut cx, "blotter::top", None);
        act(&mut cx, "blotter::visual", None);
        act(&mut cx, "blotter::down", Some(1));
        act(&mut cx, "blotter::yank", None);
        let clip = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(
            clip.as_deref(),
            Some("lhu / underlying_ref\tdelta01\tdaily_trading_pnl\n\t9\t7\n  L1\t5\t7\n")
        );
        assert!(
            matches!(
                h.tile
                    .read_with(&cx, |t, cx| t.table().read(cx).delegate().mode),
                Mode::Normal
            ),
            "yank leaves visual"
        );
        assert!(!act(&mut cx, "workspace::focus_left", None), "not ours");
    }

    #[gpui::test]
    fn find_jumps_under_vim_and_narrows_under_fzf(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        h.tile
            .update(&mut cx, |t, cx| t.find(FindEvent::Changed("l2".into()), cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row),
            2
        );
        h.tile
            .update(&mut cx, |t, cx| t.find(FindEvent::Cancelled, cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row),
            0,
            "back to the origin"
        );

        h.tile
            .update(&mut cx, |t, _| t.find_style.set(FindStyle::Fzf));
        h.tile
            .update(&mut cx, |t, cx| t.find(FindEvent::Changed("l".into()), cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()),
            vec![1, 2]
        );
        h.tile.update(&mut cx, |t, cx| {
            t.find(FindEvent::Committed("l".into()), cx)
        });
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()),
            vec![1, 2],
            "Enter keeps it"
        );
        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::escape".into()), None, cx)
        });
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()),
            vec![0, 1, 2]
        );
    }

    #[gpui::test]
    fn scope_asof_and_sort_commands_and_completions(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile
            .update(&mut cx, |t, cx| t.command("scope lhu = 'L1'", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.scope.expression.is_some());
        let err = h
            .tile
            .update(&mut cx, |t, cx| t.command("scope lhu = ", cx))
            .unwrap_err();
        assert!(err.contains("at column"), "{err}");
        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, geode_core::query::AsOf::At(_)));
        h.tile
            .update(&mut cx, |t, cx| t.command("live", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live());
        h.tile
            .update(&mut cx, |t, cx| t.command("scope undo", cx).unwrap());
        let _ = next_query(&h.requests);

        let err = h
            .tile
            .update(&mut cx, |t, cx| t.command("sort nonesuch", cx))
            .unwrap_err();
        assert!(err.contains("nonesuch"));
        let words = h.tile.read_with(&cx, |t, cx| t.completions("sort ", 5, cx));
        assert_eq!(words, vec!["clear", "daily_trading_pnl", "delta01"]);
        let words = h.tile.read_with(&cx, |t, cx| t.completions("view ", 5, cx));
        assert_eq!(words, vec!["tree", "wide"]);
        let state = h.tile.read_with(&cx, |t, _| t.serialize());
        assert_eq!(state["view"].as_str(), Some("tree"));
        assert_eq!(state["unscoped"].as_bool(), Some(false));
    }

    #[gpui::test]
    fn the_tile_paints_and_a_row_click_moves_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        assert!(cx.debug_bounds("tile-content-7").is_some());
        assert!(cx.debug_bounds("blotter-header-7").is_some());
        let table = h.tile.read_with(&cx, |t, _| t.table().clone());
        table.update(&mut cx, |t, cx| t.set_selected_row(2, cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row),
            2
        );
    }

    /// The shell cannot depend on `geode-blotter` (layering: shell never
    /// depends on a module), so `geode_shell::defaults` carries its own
    /// copy of these ids to reserve, ahead of `BlotterFactory::
    /// register_actions`, so `BUILTIN_KEYMAP`'s `blotter::*` bindings
    /// are never dropped as unregistered. This pins the two lists
    /// identical.
    #[test]
    fn the_shells_reserved_blotter_actions_match_ours() {
        let ours: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ours, geode_shell::defaults::BLOTTER_ACTIONS.to_vec());
    }
}
