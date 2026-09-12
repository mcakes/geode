//! One blotter tile (Phase 3 spec §6.5, §6.7, §6.8): observes the frame,
//! submits keyed queries through `DataHandle`, applies outcomes, records
//! timing, and paints the header strip, the table, and the footer.

use crate::core::commands::{Command, Vocabulary, completions, parse, parse_as_of};
use crate::core::cursor::{Mode, selection};
use crate::core::find::FindState;
use crate::core::flatten::{SortOrder, SortSpec};
use crate::core::plan::ColumnKind;
use crate::core::yank::tsv;
use crate::delegate::BlotterDelegate;
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::query::{AsOf, QueryKey, QueryOutcome};
use geode_core::schema::SchemaSpec;
use geode_core::scope::{Scope, parse_expr};
use geode_core::snapshot::Snapshot;
use geode_core::view::ViewSpec;
use geode_data::{DataHandle, QueryParams};
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::linenumbers::{LineNumbers, UiSettings};
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
use std::sync::Arc;
use std::time::{Duration, Instant};

/// After this long without a result the header shows an in-flight glyph
/// (foundation §7.1's 50–200 ms affordance).
const IN_FLIGHT_AFTER: Duration = Duration::from_millis(50);

/// Spec §6.5's default for `[app] blotter.stale_after`, until Task 8
/// reads the real config value. Exposed so `BlotterFactory::new`'s
/// caller (`geode-app`) has a sensible value to pass before then.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(15 * 60);

pub const ACTIONS: &[(&str, &str)] = &[
    ("blotter::down", "Cursor down"),
    ("blotter::up", "Cursor up"),
    ("blotter::left", "Cursor left"),
    ("blotter::right", "Cursor right"),
    ("blotter::top", "Cursor to top"),
    ("blotter::bottom", "Cursor to bottom"),
    ("blotter::page_down", "Half page down"),
    ("blotter::page_up", "Half page up"),
    ("blotter::page_down_full", "Page down"),
    ("blotter::page_up_full", "Page up"),
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
    (
        "blotter::sort_cycle_abs",
        "Sort by cursor column's magnitude",
    ),
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
    /// The schema and derived dimensions `:filter` validates a tile's
    /// scope against (Phase 4a §3.7) — shared with every other tile the
    /// same way `views` is, refreshed on `ConfigReloaded`.
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    pub find_style: Rc<Cell<FindStyle>>,
    /// `[app] blotter.stale_after` (spec §6.5; 15m default, read by the
    /// app in Task 8) — carried in exactly like `find_style` so a config
    /// reload can update every open tile without recreating it.
    pub stale_after: Rc<Cell<Duration>>,
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
    /// An outcome that arrived while the frame's flip barrier (Phase 4
    /// §3.10) still wants this tile's key — held here, not applied, until
    /// `promote` (driven by `flip` bumping in `on_frame_changed`, or by
    /// this tile's own `deliver` when its arrival happened to be the one
    /// that emptied the barrier) puts it through `apply` exactly like an
    /// un-barriered outcome would have been. `None` once promoted, and
    /// also `None` the whole time for an outcome that never had to wait
    /// (no barrier open, or the barrier's versions don't match).
    ///
    /// Stamped with the `acted` versions it was delivered for (fix round
    /// 1, Finding 1): a second scope/grouping/as-of mutation within the
    /// same 250ms window replaces the barrier before this tile's own
    /// fresh requery (for the newer versions) lands, so a `flip` bump
    /// from the *newer* barrier releasing must not promote a snapshot
    /// staged for the *older* one — `promote` checks the stamp against
    /// the frame's current versions and drops a stale entry rather than
    /// painting it. `requery` also clears this at its own top: a fresh
    /// query always supersedes whatever was staged before it, whether or
    /// not this particular check would have caught it.
    staged: Option<(Arc<Snapshot>, Vec<String>, FrameVersions)>,
    /// `versions().flip` as of the last promotion (Phase 4 §3.10) — this
    /// tile's own half of the bump, the same shape as `acted` above but
    /// for "have I applied what this flip released" rather than "what did
    /// I last query for".
    last_flip: u64,
}

impl BlotterTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: Entity<Frame>,
        data: DataHandle,
        views: Rc<RefCell<Vec<ViewSpec>>>,
        schema: Rc<RefCell<SchemaSpec>>,
        dims: Rc<RefCell<DerivedDimensions>>,
        find_style: Rc<Cell<FindStyle>>,
        stale_after: Rc<Cell<Duration>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_name = restored
            .and_then(|t| t.get("view").and_then(|v| v.as_str()).map(str::to_string))
            .filter(|n| views.borrow().iter().any(|v| &v.name == n))
            .or_else(|| {
                // Phase 4b M6: the view flagged `default` (a top-level
                // `default = "<name>"` key in the views doc) wins over
                // "just take the first one" — `ViewSpec::from_doc` only
                // sorts by name when no view carries the flag, so this
                // is deterministic either way, but an explicit default
                // must win when the author bothered to name one.
                let views = views.borrow();
                views
                    .iter()
                    .find(|v| v.is_default)
                    .or_else(|| views.first())
                    .map(|v| v.name.clone())
            })
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
        // `filter.expr`/`filter.text` (Phase 4a §3.7): a restored
        // expression that no longer parses (e.g. hand-edited, or a
        // column since removed) drops the whole filter rather than
        // half-applying it — logged (`geode::shell`, warn) since a fresh
        // tile has nowhere inline to report it.
        let tile_scope = restored
            .and_then(|t| t.get("filter"))
            .and_then(|v| v.as_table())
            .and_then(|f| {
                let mut scope = Scope::default();
                if let Some(expr_str) = f.get("expr").and_then(|v| v.as_str()) {
                    match parse_expr(expr_str) {
                        Ok(expr) => scope.expression = Some(expr),
                        Err(e) => {
                            tracing::warn!(
                                target: "geode::shell",
                                "restored filter.expr '{expr_str}' failed to parse at column {}: {} — filter dropped",
                                e.caret + 1,
                                e.message
                            );
                            return None;
                        }
                    }
                }
                if let Some(text) = f.get("text").and_then(|v| v.as_str()) {
                    scope.text = (!text.is_empty()).then(|| text.to_string());
                }
                Some(scope)
            })
            .unwrap_or_default();

        // `[ui] line_numbers` arrives through the shell's `UiSettings`
        // global (see `geode_shell::linenumbers`'s module doc for why a
        // global and not the `ConfigReloaded` route `find_style` rides):
        // read once here, then on every publish through `observe_global`.
        let line_numbers = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
        let table = cx.new(|cx| {
            let mut delegate = BlotterDelegate::new();
            delegate.line_numbers = line_numbers;
            TableState::new(delegate, window, cx)
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
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();

        BlotterTile {
            tile,
            frame,
            data,
            views,
            schema,
            dims,
            find_style,
            stale_after,
            table,
            view_name,
            pin,
            unscoped,
            tile_scope,
            acted: None,
            tag: 0,
            last_grouping: Vec::new(),
            in_flight: None,
            delivered_at: None,
            visible: false,
            error: None,
            find: None,
            staged: None,
            last_flip: 0,
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

    /// `:filter` narrows the tile's own scope layer, so it must be valid
    /// against this tile's dataset (spec §10.1) the same way the frame's
    /// `:scope` is validated in the shell — an unknown column or a bad
    /// operator on a derived dimension is a user error reported at the
    /// caret/column, not a silent no-op or a compiler error surfaced far
    /// downstream. `Ok(())` when the view or its dataset isn't resolvable
    /// (nothing to validate against yet — `requery`'s own "view is not
    /// configured" error already covers that case).
    fn validate_tile_scope(&self, scope: &Scope) -> Result<(), String> {
        let Some(view) = self.view() else {
            return Ok(());
        };
        let schema = self.schema.borrow();
        let Some(dataset) = schema.dataset(&view.dataset) else {
            return Ok(());
        };
        let dims = self.dims.borrow();
        match scope.validate(dataset, &dims).into_iter().next() {
            Some(d) => Err(d.message),
            None => Ok(()),
        }
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

    /// Which counters this tile follows (§4.1). Deliberately does not
    /// compare `now.flip`/`acted.flip` (Phase 4 §3.10): `flip` never means
    /// "requery" — it means "a staged snapshot this tile already has may
    /// now be promoted", which `on_frame_changed` checks on its own before
    /// ever reaching this method.
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
        // Phase 4 §3.10: a flip released (or this tile never had to wait
        // and `staged` is empty, a no-op) — promote whatever is staged
        // regardless of visibility, so a tile hidden between staging and
        // the flip is never left showing stale data once it comes back.
        let flip = self.frame.read(cx).versions().flip;
        if flip != self.last_flip {
            self.last_flip = flip;
            self.promote(cx);
        }
        if !self.visible {
            return;
        }
        let now = self.frame.read(cx).versions();
        if self.follows_changed(now) {
            self.requery(cx);
        } else {
            // A pinned tile under a grouping change, or an unscoped tile
            // under a scope change, does not requery — but it still sits
            // in an open barrier's key set (§3.10, `ShellView::
            // visible_tile_keys` does not know which tiles will follow).
            // Left unanswered, it would hold every other tile open until
            // `FLIP_DEADLINE`, for no reason: it has nothing new coming.
            let key = QueryKey(self.tile.0);
            if self.frame.read(cx).barrier_wants(key, now) {
                self.frame.update(cx, |f, cx| {
                    if f.arrived(key, now) {
                        cx.notify();
                    }
                });
            }
        }
        cx.notify();
    }

    /// Apply a staged snapshot, if any (Phase 4 §3.10) — `deliver` when
    /// its own arrival didn't empty the barrier, or `on_frame_changed`
    /// once `flip` shows it did. A no-op when nothing is staged, so
    /// calling it on every `flip` bump costs nothing for a tile that
    /// never had to wait.
    ///
    /// Fix round 1, Finding 1: a staged snapshot is only ever valid for
    /// the `(scope, grouping, as_of)` triple it was staged under. A
    /// second mutation within the same barrier window replaces it with
    /// one over newer versions before this tile's own fresh requery for
    /// those newer versions lands — when that happens, the `flip` bump
    /// that eventually releases the newer barrier must not promote a
    /// snapshot staged for the older one. Dropping it here keeps
    /// whatever is already on screen (last-good); the tile's own
    /// `requery` for the newer versions (already in flight by the time
    /// this runs — `follows_changed` fires in the same `on_frame_changed`
    /// pass) will paint the real answer when it lands.
    fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((snapshot, grouping, versions)) = self.staged.take() else {
            return;
        };
        let now = self.frame.read(cx).versions();
        if versions.same_flip_identity(now) {
            self.apply(snapshot, grouping, cx);
        }
    }

    /// Put a snapshot through the table exactly as an un-barriered
    /// `deliver` always has: `apply_snapshot`, refresh, re-clamp the
    /// cursor to the (possibly reshaped) row set, and record when this
    /// landed. Never reorders or re-reads `snapshot` — it applies exactly
    /// what the query pool handed back, at the grouping it was queried
    /// under.
    fn apply(&mut self, snapshot: Arc<Snapshot>, grouping: Vec<String>, cx: &mut Context<Self>) {
        if let Some(view) = self.view() {
            self.table.update(cx, |t, cx| {
                t.delegate_mut().apply_snapshot(snapshot, &view, &grouping);
                t.refresh(cx);
                let row = t.delegate().cursor.row;
                t.set_selected_row(row, cx);
            });
        }
        self.delivered_at = Some(Instant::now());
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        // Fix round 1, Finding 1: a fresh query always supersedes
        // whatever was staged before it, whether or not it was already
        // stale for the barrier `promote`'s own version check would
        // otherwise have caught it against.
        self.staged = None;
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
        let key = QueryKey(self.tile.0);
        let acted = self.acted.unwrap_or_default();
        match outcome.snapshot {
            Ok(snapshot) => {
                self.error = None;
                // Phase 4 §3.10: if a flip barrier is open and still
                // wants this key, stage rather than apply — this tile
                // must not paint the new scope/grouping/as-of before
                // every other following tile is ready to as well.
                let wants = self.frame.read(cx).barrier_wants(key, acted);
                if wants {
                    self.staged = Some((snapshot, self.last_grouping.clone(), acted));
                    // `arrived` may itself empty the barrier right here —
                    // when it does, promote immediately rather than
                    // waiting for the `flip` bump to reach this tile's
                    // own `on_frame_changed` on a later notify pass.
                    let released = self.frame.update(cx, |f, cx| {
                        let r = f.arrived(key, acted);
                        if r {
                            cx.notify();
                        }
                        r
                    });
                    if released {
                        self.promote(cx);
                    }
                } else {
                    self.apply(snapshot, self.last_grouping.clone(), cx);
                }
            }
            Err(e) => {
                self.error = Some(e);
                // A failed outcome still counts as arrival (§3.10): one
                // broken tile must never hold every other tile open until
                // the deadline.
                self.frame.update(cx, |f, cx| {
                    if f.arrived(key, acted) {
                        cx.notify();
                    }
                });
            }
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

    /// The shell republished `UiSettings`: mirror `line_numbers` into
    /// the delegate and, if it changed, refresh the table's column
    /// groups (the tree column's width includes the gutter — `TableState`
    /// caches `column()`'s answer until `refresh`) and repaint.
    fn on_ui_settings(&mut self, cx: &mut Context<Self>) {
        let mode = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
        let changed = self.with_delegate(cx, |d| {
            let changed = d.line_numbers != mode;
            d.line_numbers = mode;
            changed
        });
        if changed {
            self.table.update(cx, |t, cx| {
                t.refresh(cx);
                cx.notify();
            });
            cx.notify();
        }
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
            // The step sizes are `vimnav`'s own convention, shared with
            // every dialog list: `ctrl+d`/`ctrl+u` ±5, `ctrl+f`/`ctrl+b`
            // (and `pagedown`/`pageup`) ±10 — fixed offsets, not vim's
            // viewport-relative scroll, since the count prefix already
            // multiplies them.
            "down" | "up" | "top" | "bottom" | "page_down" | "page_up" | "page_down_full"
            | "page_up_full" => {
                let cmd = match name {
                    "down" => NavCommand::Move(1),
                    "up" => NavCommand::Move(-1),
                    "top" => NavCommand::Top,
                    "bottom" => NavCommand::Bottom,
                    "page_down" => NavCommand::Move(5),
                    "page_up" => NavCommand::Move(-5),
                    "page_down_full" => NavCommand::Move(10),
                    _ => NavCommand::Move(-10),
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
            // `s` walks the signed cycle and `S` the absolute one; the
            // step itself is `SortOrder::cycle`, which also keeps `S`
            // inert on a column with no magnitude.
            "sort_cycle" | "sort_cycle_abs" => {
                let absolute = name == "sort_cycle_abs";
                self.with_delegate(cx, |d| {
                    let col = d.cursor.col;
                    if col == 0 {
                        return;
                    }
                    let measure = d.is_measure(col);
                    let current = d.sort.filter(|s| s.column == col).map(|s| s.order);
                    let next = SortOrder::cycle(current, absolute, measure);
                    if next == current {
                        return;
                    }
                    d.sort = next.map(|order| SortSpec { column: col, order });
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
            Command::FilterExpr(text) => {
                let expr = parse_expr(&text)
                    .map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
                let mut scope = self.tile_scope.clone();
                scope.expression = Some(expr);
                self.validate_tile_scope(&scope)?;
                self.tile_scope = scope;
                self.requery(cx);
            }
            Command::FilterText(words) => {
                let mut scope = self.tile_scope.clone();
                scope.text = (!words.trim().is_empty()).then_some(words);
                self.tile_scope = scope;
                self.requery(cx);
            }
            Command::FilterClear => {
                self.tile_scope = Scope::default();
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
            Command::ScopeRedo => {
                let redone = self.frame.update(cx, |f, cx| {
                    let r = f.redo_scope();
                    cx.notify();
                    r
                });
                if !redone {
                    return Err("nothing to redo".into());
                }
            }
            Command::ScopeDrop(d) => {
                let dropped = self.frame.update(cx, |f, cx| {
                    let r = f.drop_dimension(&d);
                    if r {
                        cx.notify();
                    }
                    r
                });
                if !dropped {
                    return Err(format!("no selection on '{d}'"));
                }
            }
            Command::ScopeSave(name) => {
                self.frame.update(cx, |f, cx| {
                    let r = f.save_scope(&name);
                    cx.notify();
                    r
                })?;
            }
            Command::ScopeLoad(name) => {
                self.frame.update(cx, |f, cx| {
                    let r = f.load_scope(&name);
                    cx.notify();
                    r
                })?;
            }
            Command::AsOf(text) => {
                let at = parse_as_of(&text, chrono::Utc::now())?;
                self.frame.update(cx, |f, cx| {
                    if f.set_as_of(AsOf::At(at)) {
                        cx.notify();
                    }
                });
            }
            Command::AsOfUndo => {
                let undone = self.frame.update(cx, |f, cx| {
                    let r = f.undo_as_of();
                    cx.notify();
                    r
                });
                if !undone {
                    return Err("no previous as-of".into());
                }
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
            Command::Sort { column, order } => {
                let found = self.with_delegate(cx, |d| {
                    let col = d
                        .plan
                        .as_ref()?
                        .columns
                        .iter()
                        .position(|c| c.name == column)?;
                    // A text column has no magnitude: `abs` on it is its
                    // signed direction, in the state as on the screen.
                    let order = order.on_column(d.is_measure(col));
                    d.sort = Some(SortSpec { column: col, order });
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
        // Every column the tile's dataset carries as a dimension at any
        // grain it has (Phase 4a: a dimension can now name the grain
        // that carries it, so this can no longer be read off the column
        // plan/view above, which only ever lists what's *displayed*),
        // plus every derived dimension — `columns` is what `sort` can
        // rank; `dimensions` is what `group`/`scope drop`/`scope`/
        // `filter` complete from (`core::commands::completions`).
        let mut dimensions: Vec<String> = match self.view() {
            Some(v) => {
                let schema = self.schema.borrow();
                match schema.dataset(&v.dataset) {
                    Some(ds) => {
                        let mut names: std::collections::HashSet<&str> =
                            std::collections::HashSet::new();
                        for g in ds.grains() {
                            names.extend(ds.dimensions_at(g));
                        }
                        ds.columns
                            .iter()
                            .filter(|c| names.contains(c.name.as_str()))
                            .map(|c| c.name.clone())
                            .collect()
                    }
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        let mut seen: std::collections::HashSet<String> = dimensions.iter().cloned().collect();
        for d in self.dims.borrow().all() {
            if seen.insert(d.name.clone()) {
                dimensions.push(d.name.clone());
            }
        }
        let views = self.views.borrow().iter().map(|v| v.name.clone()).collect();
        let scopes = self.frame.read(cx).saved_scopes().keys().cloned().collect();
        completions(
            line,
            cursor,
            &Vocabulary {
                columns,
                dimensions,
                views,
                scopes,
            },
        )
    }

    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed(query) => {
                if self.find.is_none() {
                    let origin = self.table.read(cx).delegate().cursor.row;
                    self.find = Some(FindState::begin(self.find_style.get(), origin));
                }
                let style = self.find_style.get();
                // Fzf narrows progressively: every keystroke must match
                // against the full `visible` list (`set_narrowed`'s own
                // domain), never against the previous keystroke's already-
                // narrowed `shown` — otherwise the second keystroke's
                // match positions land in the wrong domain and a
                // shortened query can never widen the result back out
                // (review round 1, Finding 1). Vim never narrows, so
                // `shown` and `visible` agree for it either way; keep it
                // on `shown_texts()` to match its own cursor-jump domain
                // exactly (`cursor.to_row` elsewhere in this file always
                // takes a position into `shown`).
                let texts = if style == FindStyle::Fzf {
                    self.table.read(cx).delegate().visible_texts()
                } else {
                    self.table.read(cx).delegate().shown_texts()
                };
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
        if !self.tile_scope.is_empty() {
            let mut filter = toml::Table::new();
            if let Some(expr) = &self.tile_scope.expression {
                filter.insert("expr".into(), toml::Value::String(expr.to_string()));
            }
            if let Some(text) = &self.tile_scope.text {
                filter.insert("text".into(), toml::Value::String(text.clone()));
            }
            t.insert("filter".into(), toml::Value::Table(filter));
        }
        t
    }

    /// Whether a per-dataset freshness reading (§6.5) is old enough to
    /// warrant the header's stale marker, per this tile's configured
    /// `stale_after` (review round 1, Finding 2: was a hardcoded 15m).
    pub(crate) fn is_stale(&self, as_of: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
        as_of
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| {
                now.signed_duration_since(t.with_timezone(&chrono::Utc))
                    .to_std()
                    .unwrap_or_default()
                    > self.stale_after.get()
            })
    }
}

/// The `HH:MM` slice of an RFC-3339 `as_of` timestamp, for the header's
/// per-dataset freshness readout. `str::get` rather than direct
/// indexing (`&t[11..16]`), so an `as_of` string shorter than 11 bytes
/// (start > end — a panic on direct indexing, not just a truncation) or
/// one whose 11/16 byte offsets don't land on a char boundary both fall
/// back to the whole string instead of panicking on the render thread —
/// a malformed freshness timestamp must never be able to take the
/// render thread down with it.
fn short_time(t: &str) -> &str {
    t.get(11..16.min(t.len())).unwrap_or(t)
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
        if !self.tile_scope.is_empty() {
            header = header.child(
                div()
                    .text_color(theme.warning_foreground)
                    .bg(theme.warning.opacity(0.25))
                    .px_1()
                    .rounded(px(3.))
                    .debug_selector(|| format!("blotter-filtered-{}", self.tile.0))
                    .child("filtered"),
            );
        }
        if let Some(snapshot) = &snapshot {
            let p = snapshot.provenance();
            let mut datasets: Vec<_> = p.datasets.iter().collect();
            datasets.sort_by(|a, b| a.as_of.cmp(&b.as_of));
            let now = chrono::Utc::now();
            for f in datasets {
                let text = match &f.as_of {
                    Some(t) => format!("{} {}", f.dataset, short_time(t)),
                    None => format!("{} —", f.dataset),
                };
                let stale = self.is_stale(f.as_of.as_deref(), now);
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

#[test]
fn short_time_falls_back_to_the_whole_string_instead_of_panicking() {
    assert_eq!(short_time("2026-08-30T14:32:00Z"), "14:32");
    // Shorter than the 11-byte offset the slice starts at: direct
    // indexing (`&t[11..16.min(t.len())]`) would panic here (start >
    // end); `short_time` falls back to the whole string.
    assert_eq!(short_time("2026"), "2026");
    assert_eq!(short_time(""), "");
    // Exactly 11 bytes: the slice is `11..11`, valid but empty.
    assert_eq!(short_time("2026-08-30T"), "");
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::groupings::GroupingSlots;
    use geode_core::query::{QueryKey, QueryOutcome};
    use geode_core::scopes::SavedScopes;
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::frame::{FLIP_DEADLINE, Frame, Publish};
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

    /// [`views`], plus a top-level `default = "<name>"` header (Phase 4b
    /// Task 1 fix round 1, MIN-4) — for pinning `BlotterTile::new`'s own
    /// half of M6 (`.find(|v| v.is_default)`), which had no test of its
    /// own: the two M6 tests in `geode-core::view` both pin `ViewSpec::
    /// from_doc`'s sort/flag, not the half a fresh tile actually feels.
    fn views_with_explicit_default(default: &str) -> Vec<ViewSpec> {
        let text = format!(
            "default = \"{default}\"\n[tree]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[tree.columns]]\nname = \"delta01\"\n[[tree.columns]]\nname = \"daily_trading_pnl\"\n[wide]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[wide.columns]]\nname = \"delta01\"\n"
        );
        let doc = merge_docs("views", &[LayerDoc::builtin("views", &text).unwrap()]);
        ViewSpec::from_doc(&doc).0
    }

    /// The `d` dataset `views()`'s "tree"/"wide" views point at —
    /// `validate_tile_scope`'s target for the `:filter` tests below.
    /// `model_code` is a carried dimension (`grain = "instrument"`) so a
    /// `:filter model_code = 'EURP'` validates as a legitimate column,
    /// same shape as `geode_core::scope::mod`'s own fixture dataset.
    fn schema() -> SchemaSpec {
        let text = "[d.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [d.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [d.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                     [d.columns.counterparty]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                     [d.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                     [d.columns.underlying_ref]\ntype = \"utf8\"\nrole = \"dimension\"\ntextual = true\n\
                     [d.columns.model_code]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"instrument\"\n\
                     [d.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n\
                     [d.columns.daily_trading_pnl]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n";
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema
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

    /// Same shape as [`snapshot`] (root; two depth-1 groups; one depth-2
    /// child) but with different `lhu` labels — the flip barrier e2e test
    /// (`two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_
    /// barrier`) tells "still showing the old snapshot" from "promoted to
    /// the new one" by comparing `shown_texts()` against this fixture's
    /// labels rather than `snapshot()`'s.
    fn snapshot2() -> Arc<Snapshot> {
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
                        Some("M1".into()),
                        Some("M2".into()),
                        Some("M1".into()),
                    ]),
                ),
                (
                    meta("underlying_ref", vec![Attribution::Additive; 3]),
                    TestColumn::Dict(vec![None, None, None, Some("NDX".into())]),
                ),
                (
                    meta("row_depth", vec![Attribution::Additive; 3]),
                    TestColumn::I32(vec![0, 1, 1, 2]),
                ),
                (
                    meta("delta01", vec![Attribution::Additive; 3]),
                    TestColumn::F64(vec![Some(90.0), Some(50.0), Some(40.0), Some(50.0)]),
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
                    TestColumn::F64(vec![Some(70.0), Some(70.0), Some(70.0), None]),
                ),
            ],
            2,
        ))
    }

    /// A third distinct payload (fix round 1, Finding 1's regression
    /// test): the stale-staged-snapshot race needs three tellable-apart
    /// generations — the pre-V1 baseline (`snapshot()`), the stale V1
    /// payload that must never paint (`snapshot2()`), and the real V2
    /// payload that must (this one).
    fn snapshot3() -> Arc<Snapshot> {
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
                        Some("N1".into()),
                        Some("N2".into()),
                        Some("N1".into()),
                    ]),
                ),
                (
                    meta("underlying_ref", vec![Attribution::Additive; 3]),
                    TestColumn::Dict(vec![None, None, None, Some("RTY".into())]),
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
        open_with(cx, None)
    }

    /// Same as [`open`], but a `restored` record (§3.7's `filter.expr`/
    /// `filter.text` round trip) is threaded straight into `BlotterTile::
    /// new`, exactly as `BlotterFactory::create` does for a session
    /// restore.
    fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_in(cx, restored, gpui::WindowOptions::default())
    }

    /// [`open_with`] in a window of the caller's choosing — a narrow one
    /// is the only way to make the table actually scroll horizontally in
    /// a test, which the pinned tree column's test needs.
    fn open_in(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        options: gpui::WindowOptions,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (data, requests) = DataHandle::for_tests();
        let window = cx
            .update(|cx| {
                cx.open_window(options, |window, cx| {
                    let frame = cx.new(|_| Frame::new(slots(), SavedScopes::new(), None));
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
                                data.clone(),
                                Rc::new(RefCell::new(views())),
                                Rc::new(RefCell::new(schema())),
                                Rc::new(RefCell::new(DerivedDimensions::default())),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                Rc::new(Cell::new(DEFAULT_STALE_AFTER)),
                                restored,
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

    /// Same as [`open_with`], but the views doc is the caller's own
    /// rather than the fixed [`views`] fixture — for
    /// [`views_with_explicit_default`] (Phase 4b Task 1 fix round 1,
    /// MIN-4).
    fn open_with_views(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        views: Vec<ViewSpec>,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (data, requests) = DataHandle::for_tests();
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(slots(), SavedScopes::new(), None));
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
                                data.clone(),
                                Rc::new(RefCell::new(views)),
                                Rc::new(RefCell::new(schema())),
                                Rc::new(RefCell::new(DerivedDimensions::default())),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                Rc::new(Cell::new(DEFAULT_STALE_AFTER)),
                                restored,
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

    /// Phase 4b Task 1 fix round 1, MIN-4: a fresh tile (nothing
    /// restored) must open on the view flagged `default`, not the one
    /// that happens to sort first by name — "wide" is flagged here,
    /// while "tree" < "wide" alphabetically, so a regression that drops
    /// `BlotterTile::new`'s `.find(|v| v.is_default)` would silently
    /// open on "tree" instead.
    #[gpui::test]
    fn a_fresh_tile_opens_on_the_explicit_default_view_not_the_alphabetical_first(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, vcx) = open_with_views(cx, None, views_with_explicit_default("wide"));
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(state["view"].as_str(), Some("wide"));
    }

    /// Two tiles sharing one frame, one `DataHandle`/`Receiver<Request>`
    /// pair (distinguished by `QueryKey`, exactly like production's one
    /// shared handle across every tile) — for `:filter`'s "narrows only
    /// this tile" tests.
    struct TwoHarness {
        a: Entity<BlotterTile>,
        b: Entity<BlotterTile>,
        requests: Receiver<Request>,
    }

    struct TwoHost {
        a: Entity<BlotterTile>,
        b: Entity<BlotterTile>,
    }
    impl gpui::Render for TwoHost {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.a.clone())
                .child(self.b.clone())
        }
    }

    fn open_two(cx: &mut gpui::TestAppContext) -> (TwoHarness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (data, requests) = DataHandle::for_tests();
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame = cx.new(|_| Frame::new(slots(), SavedScopes::new(), None));
                    let views = Rc::new(RefCell::new(views()));
                    let schema = Rc::new(RefCell::new(schema()));
                    let dims = Rc::new(RefCell::new(DerivedDimensions::default()));
                    cx.new(|cx| {
                        let a = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
                                data.clone(),
                                views.clone(),
                                schema.clone(),
                                dims.clone(),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                Rc::new(Cell::new(DEFAULT_STALE_AFTER)),
                                None,
                                window,
                                cx,
                            )
                        });
                        let b = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(8),
                                frame.clone(),
                                data.clone(),
                                views.clone(),
                                schema.clone(),
                                dims.clone(),
                                Rc::new(Cell::new(FindStyle::Vim)),
                                Rc::new(Cell::new(DEFAULT_STALE_AFTER)),
                                None,
                                window,
                                cx,
                            )
                        });
                        TwoHost { a, b }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (a, b) = window
            .root(&mut vcx)
            .unwrap()
            .read_with(&vcx, |h, _| (h.a.clone(), h.b.clone()));
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (TwoHarness { a, b, requests }, vcx)
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
        deliver_to(&h.tile, QueryKey(7), cx, tag, snapshot);
    }

    /// Same as [`deliver`], generalized over which tile and `QueryKey` —
    /// the two-tile flip barrier test delivers to A and B by their own
    /// keys rather than the single-tile `Harness`'s fixed `QueryKey(7)`.
    fn deliver_to(
        tile: &Entity<BlotterTile>,
        key: QueryKey,
        cx: &mut gpui::VisualTestContext,
        tag: u64,
        snapshot: Result<Arc<Snapshot>, String>,
    ) {
        tile.update(cx, |t, cx| {
            t.deliver(
                QueryOutcome {
                    key,
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

    /// The tree text of every un-narrowed visible row (`shown_texts`,
    /// `crate::delegate::BlotterDelegate`) — what the flip barrier e2e
    /// test compares to tell `snapshot()`'s labels from `snapshot2()`'s.
    fn shown_texts(tile: &Entity<BlotterTile>, cx: &gpui::VisualTestContext) -> Vec<String> {
        tile.read_with(cx, |t, cx| t.table().read(cx).delegate().shown_texts())
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
        // `ctrl+f`/`ctrl+b`: the ±10 step every dialog list has. Ten
        // outruns this snapshot, so it clamps to the last row, and
        // `ctrl+b` from there lands on row 0 — not on row -8.
        let row = |cx: &mut gpui::VisualTestContext| {
            h.tile
                .read_with(cx, |t, cx| t.table().read(cx).delegate().cursor.row)
        };
        assert!(act(&mut cx, "blotter::page_down_full", None));
        assert_eq!(row(&mut cx), 2, "ctrl+f clamps to the last row");
        assert!(act(&mut cx, "blotter::page_up_full", None));
        assert_eq!(row(&mut cx), 0, "ctrl+b clamps to the first row");
        act(&mut cx, "blotter::down", Some(1));
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
        // Progressive narrowing (review round 1, Finding 1): the second
        // keystroke must match against the un-narrowed `visible` list,
        // not against the previous keystroke's already-narrowed `shown`
        // — otherwise "l2"'s match position lands in the wrong domain.
        h.tile
            .update(&mut cx, |t, cx| t.find(FindEvent::Changed("l2".into()), cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()),
            vec![2],
            "narrows further to just L2, not L1"
        );
        h.tile
            .update(&mut cx, |t, cx| t.find(FindEvent::Changed("l".into()), cx));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone()),
            vec![1, 2],
            "a shortened query widens back out, not stuck within the prior narrow"
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

    /// `s` walks asc → desc → clear and `S` abs desc → abs asc → clear on
    /// the cursor's measure column; each key starts its own cycle afresh
    /// from the other's order; neither touches the tree column; the
    /// header says `|x|` only while an absolute order is showing; and
    /// `:sort <col> abs …` reaches the same state by typing.
    #[gpui::test]
    fn s_and_shift_s_cycle_signed_and_absolute_sorts(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let act = |cx: &mut gpui::VisualTestContext, id: &str| {
            h.tile
                .update(cx, |t, cx| t.dispatch(&ActionId(id.into()), None, cx))
        };
        let sort = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                let d = t.table().read(cx).delegate();
                d.sort.map(|s| (s.column, s.order))
            })
        };
        let header = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                let table = t.table().read(cx);
                gpui_component::table::TableDelegate::column(table.delegate(), 1, cx)
                    .name
                    .to_string()
            })
        };

        // On the tree column both keys are inert.
        assert!(act(&mut cx, "blotter::sort_cycle"));
        assert!(act(&mut cx, "blotter::sort_cycle_abs"));
        assert_eq!(sort(&mut cx), None);

        act(&mut cx, "blotter::right");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::Asc)));
        assert_eq!(header(&mut cx), "delta01");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::Desc)));
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), None);

        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsDesc)));
        assert_eq!(header(&mut cx), "delta01 |x|");
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsAsc)));
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(sort(&mut cx), None);
        assert_eq!(header(&mut cx), "delta01");

        // Crossing over: `s` from an absolute order restarts at asc, `S`
        // from a signed order restarts at abs desc.
        act(&mut cx, "blotter::sort_cycle_abs");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::Asc)));
        act(&mut cx, "blotter::sort_cycle");
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsDesc)));

        h.tile.update(&mut cx, |t, cx| {
            t.command("sort daily_trading_pnl abs asc", cx).unwrap()
        });
        assert_eq!(sort(&mut cx), Some((2, SortOrder::AbsAsc)));
        assert_eq!(
            header(&mut cx),
            "delta01",
            "the marker follows the sort column"
        );
        h.tile
            .update(&mut cx, |t, cx| t.command("sort delta01 abs", cx).unwrap());
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsDesc)));
        h.tile
            .update(&mut cx, |t, cx| t.command("sort clear", cx).unwrap());
        assert_eq!(sort(&mut cx), None);
    }

    /// `:filter` narrows through `tile_scope`, composed into the query's
    /// scope by `effective_scope`'s tile argument — never by
    /// post-filtering rows — so it must reach only the tile that set it.
    /// Two tiles share one frame; `:filter` on A never touches the frame,
    /// so B (which only observes the frame) never even wakes for it.
    #[gpui::test]
    fn filter_narrows_only_this_tile_marks_it_and_round_trips_the_session(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // A's initial query
        let _ = next_query(&h.requests); // B's initial query

        h.a.update(&mut vcx, |t, cx| {
            t.command("filter model_code = 'EURP'", cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert_eq!(p.key, QueryKey(7), "only tile A requeried");
        assert_eq!(
            p.scope.expression.as_ref().map(ToString::to_string),
            Some("model_code = 'EURP'".to_string())
        );
        assert!(
            h.requests.try_recv().is_err(),
            "tile B's own scope is untouched by A's :filter, so it never requeries"
        );

        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("blotter-filtered-7").is_some(),
            "A's header carries the filtered pill"
        );
        assert!(
            vcx.debug_bounds("blotter-filtered-8").is_none(),
            "B carries no such pill"
        );

        let state = h.a.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(
            state["filter"]["expr"].as_str(),
            Some("model_code = 'EURP'")
        );

        // A new tile restored from that record has the same tile_scope.
        let mut record = toml::Table::new();
        record.insert("filter".into(), state["filter"].clone());
        let (restored, restored_cx) = open_with(cx, Some(&record));
        let restored_scope = restored
            .tile
            .read_with(&restored_cx, |t, _| t.tile_scope.clone());
        let a_scope = h.a.read_with(&vcx, |t, _| t.tile_scope.clone());
        assert_eq!(restored_scope, a_scope, "the round-tripped filter matches");

        // `:filter clear` clears and the pill goes.
        h.a.update(&mut vcx, |t, cx| t.command("filter clear", cx).unwrap());
        let p2 = next_query(&h.requests);
        assert_eq!(p2.key, QueryKey(7));
        assert!(p2.scope.expression.is_none());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(vcx.debug_bounds("blotter-filtered-7").is_none());
    }

    /// `unscoped` drops the *frame's* layer, not the tile's own — a
    /// `:filter` still narrows the tile after `:unscoped`, and none of
    /// the frame's own dimension selections leak into the query's scope.
    #[gpui::test]
    fn an_unscoped_tile_still_applies_its_own_filter(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        h.tile
            .update(&mut cx, |t, cx| t.command("unscoped", cx).unwrap());
        let _ = next_query(&h.requests);

        // Give the frame a scope too, to prove it's excluded once
        // unscoped: an unscoped tile doesn't follow the frame's scope
        // version, so this alone triggers no requery.
        h.frame.update(&mut cx, |f, cx| {
            let mut scope = f.scope().clone();
            scope.text = Some("ignored".into());
            if f.set_scope(scope) {
                cx.notify();
            }
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "unscoped: the frame's own scope change is not followed"
        );

        h.tile
            .update(&mut cx, |t, cx| t.command("filter text x", cx).unwrap());
        let p = next_query(&h.requests);
        assert_eq!(p.scope.text.as_deref(), Some("x"));
        assert!(
            p.scope.dimensions.is_empty(),
            "no frame dimensions leaked in"
        );
    }

    /// A bad `:filter` expression is a user error at the point of entry
    /// (spec §10.1), reported inline, never applied.
    #[gpui::test]
    fn filter_validates_against_the_tiles_dataset(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        let err = h
            .tile
            .update(&mut cx, |t, cx| t.command("filter nope = 1", cx))
            .unwrap_err();
        assert!(err.contains("nope"), "{err}");
        assert!(
            h.requests.try_recv().is_err(),
            "no requery on a rejected filter"
        );
        let scope_after = h.tile.read_with(&cx, |t, _| t.tile_scope.clone());
        assert!(scope_after.is_empty(), "tile_scope unchanged");
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

    /// Fixture for the two painted-cell tests below: identical to
    /// `snapshot()` above, except `delta01` is also `DeterminedNonAdditive`
    /// at the leaf depth, so SPX (row id 3) carries both markers at once
    /// — a NonAttributable `daily_trading_pnl` and a DeterminedNonAdditive
    /// `delta01`.
    fn attributed_snapshot() -> Arc<Snapshot> {
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
                    meta(
                        "delta01",
                        vec![
                            Attribution::Additive,
                            Attribution::Additive,
                            Attribution::DeterminedNonAdditive,
                        ],
                    ),
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

    /// A flat, single-level shape (root plus `n` leaves) for the C1
    /// regression test below: `n` large enough that the test window's
    /// viewport shows only a fraction of it.
    fn flat_snapshot(n: usize, delta_base: f64) -> Arc<Snapshot> {
        let meta = |name: &str| ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
        };
        let mut lhu: Vec<Option<String>> = vec![None];
        let mut depth: Vec<i32> = vec![0];
        let mut delta: Vec<Option<f64>> = vec![Some(delta_base)];
        for i in 0..n {
            lhu.push(Some(format!("L{i}")));
            depth.push(1);
            delta.push(Some(delta_base + i as f64));
        }
        Arc::new(Snapshot::for_tests(
            vec![
                (meta("lhu"), TestColumn::Dict(lhu)),
                (meta("row_depth"), TestColumn::I32(depth)),
                (meta("delta01"), TestColumn::F64(delta)),
            ],
            1,
        ))
    }

    /// I4, test 1: a `NonAttributable` cell (a NULL the compiler said was
    /// never a number here, §6.5) paints no text. gpui's test harness has
    /// no pixel/text reader, so "no text" is checked the way every other
    /// test in this module checks what painted — through the delegate's
    /// `FormatCache`, which is the sole input `render_td`'s `match
    /// cell.attribution` branches on (see that match: `NonAttributable`
    /// is the only arm that never calls `.child(text)`) — backed by
    /// `debug_bounds` proving the cell's own element painted at all.
    #[gpui::test]
    fn a_non_attributable_cells_element_has_no_text(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(attributed_snapshot()));

        // shown starts [root, L1, L2]; expand L1 to reveal SPX (row id 3,
        // already materialised — no requery needed, per
        // `motions_expansion_and_yank`).
        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::down".into()), None, cx)
        });
        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::expand".into()), None, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shown = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().shown.clone());
        assert_eq!(
            shown,
            vec![0, 1, 3, 2],
            "SPX (row id 3) is now visible under L1"
        );

        let row_ix = 2; // SPX
        let col_ix = 2; // daily_trading_pnl
        // The column's own declared attribution at this depth really is
        // NonAttributable (not merely "happens to be NULL", which would
        // also cache as `None` — `cache::cell`'s NULL check and the
        // attribution the compiler assigned are two different things
        // that agree here by construction).
        let depth_attribution = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            d.plan.as_ref().unwrap().attribution(col_ix, 2)
        });
        assert_eq!(depth_attribution, Attribution::NonAttributable);
        let cell = h.tile.read_with(&cx, |t, cx| {
            t.table()
                .read(cx)
                .delegate()
                .cache
                .get(row_ix, col_ix)
                .cloned()
        });
        assert!(
            cell.is_none(),
            "a NonAttributable NULL is never cached — render_td's own \
             early return when the cache has nothing for a cell is \
             exactly what makes its element carry no text"
        );
        let selector: &'static str =
            Box::leak(format!("blotter-cell-{row_ix}-{col_ix}").into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the cell's own element still painted (an empty div, not a \
             missing row) even though it carries no text"
        );
    }

    /// I4, test 2: a `DeterminedNonAdditive` cell carries the dagger — in
    /// practice, the delegate's cached attribution `render_td` paints the
    /// dagger from, plus the footer's `any_determined` flag it drives
    /// (see `render_td`'s `DeterminedNonAdditive` arm, which appends
    /// `DETERMINED_MARK` after the text).
    #[gpui::test]
    fn a_determined_non_additive_cells_element_carries_the_dagger(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(attributed_snapshot()));

        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::down".into()), None, cx)
        });
        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::expand".into()), None, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let row_ix = 2; // SPX
        let col_ix = 1; // delta01
        let cell = h.tile.read_with(&cx, |t, cx| {
            t.table()
                .read(cx)
                .delegate()
                .cache
                .get(row_ix, col_ix)
                .cloned()
        });
        assert_eq!(
            cell.map(|c| c.attribution),
            Some(Attribution::DeterminedNonAdditive),
            "delta01 is DeterminedNonAdditive at this leaf — render_td \
             paints the dagger for it"
        );
        assert!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().any_determined),
            "the footer's dagger legend flag follows the cached window"
        );
        let selector: &'static str =
            Box::leak(format!("blotter-cell-{row_ix}-{col_ix}").into_boxed_str());
        assert!(cx.debug_bounds(selector).is_some(), "the cell painted");
    }

    /// The tree column is pinned left (user ruling 2026-09-12): in a
    /// window too narrow for the fixture's three columns, `$` scrolls
    /// the table right so the last column ends flush with the viewport's
    /// right edge (its x moves left, and column 1 is culled), while the
    /// tree cell's painted x does not move — it is rendered in the
    /// table's fixed region, outside the scrolled one.
    #[gpui::test]
    fn the_tree_column_stays_put_when_the_table_scrolls_right(cx: &mut gpui::TestAppContext) {
        use gpui::{Bounds, WindowBounds, point, size};
        let options = gpui::WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(300.), px(240.)),
            ))),
            ..Default::default()
        };
        let (h, mut cx) = open_in(cx, None, options);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds_of = |cx: &mut gpui::VisualTestContext, sel: &'static str| {
            cx.debug_bounds(sel)
                .unwrap_or_else(|| panic!("{sel} painted"))
        };
        let tree_x = bounds_of(&mut cx, "blotter-cell-0-0").origin.x;
        let last_before = bounds_of(&mut cx, "blotter-cell-0-2");
        assert!(
            last_before.right() > px(300.),
            "sanity: the window is narrow enough that the last column overflows it ({:?})",
            last_before.right()
        );

        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::last_col".into()), None, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let last_after = bounds_of(&mut cx, "blotter-cell-0-2");
        assert!(
            last_after.origin.x < last_before.origin.x,
            "sanity: `$` scrolled the table right ({:?} -> {:?})",
            last_before.origin.x,
            last_after.origin.x
        );
        assert!(
            last_after.right() <= px(300.),
            "the last column now ends inside the window ({:?})",
            last_after.right()
        );
        assert_eq!(
            bounds_of(&mut cx, "blotter-cell-0-0").origin.x,
            tree_x,
            "the tree cell did not move: it is pinned in the fixed region"
        );
    }

    /// `[ui] line_numbers` reaches a live tile through the shell's
    /// `UiSettings` global (user ruling 2026-09-11): with the setting
    /// off no gutter element paints; publishing `rel` paints one per row
    /// on the next draw without any requery, numbered from the cursor
    /// with the cursor row showing its absolute number; and a cursor
    /// move re-derives the offsets.
    #[gpui::test]
    fn the_line_numbers_global_paints_a_gutter_on_the_next_draw(cx: &mut gpui::TestAppContext) {
        use geode_shell::linenumbers::{LineNumbers, UiSettings};
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-gutter-0").is_none(),
            "no gutter while the setting is off (the default with no global set)"
        );
        // The *painted* tree cell's width — what `TableState`'s cached
        // column groups actually laid out — not `delegate().column()`'s
        // answer, which a test could read without any refresh having
        // happened.
        let painted_tree_width = |cx: &mut gpui::VisualTestContext| -> f32 {
            f32::from(
                cx.debug_bounds("blotter-cell-0-0")
                    .expect("row 0's tree cell painted")
                    .size
                    .width,
            )
        };
        let base_width = painted_tree_width(&mut cx);

        cx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::Relative,
            })
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            h.requests.try_recv().is_err(),
            "a presentation setting never requeries"
        );
        assert!(
            cx.debug_bounds("blotter-gutter-0").is_some(),
            "the gutter painted once the global was published"
        );
        let gutter = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().gutter_px());
        assert!(gutter > 0.0, "sanity: a live gutter has width");
        assert!(
            (painted_tree_width(&mut cx) - (base_width + gutter)).abs() < 0.5,
            "the painted tree cell widened by the gutter ({base_width} + {gutter}); \
             `on_ui_settings` must `refresh` the table, since `TableState` caches \
             `column()`'s width until told otherwise"
        );
        let texts = |cx: &mut gpui::VisualTestContext| -> Vec<String> {
            h.tile.update(cx, |t, cx| {
                t.table().update(cx, |t, _| {
                    let d = t.delegate_mut();
                    (0..3)
                        .map(|r| d.gutter_text(r).map(|s| s.to_string()).unwrap_or_default())
                        .collect()
                })
            })
        };
        assert_eq!(
            texts(&mut cx),
            vec!["1", "1", "2"],
            "cursor on row 0: its absolute number, then distances"
        );

        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("blotter::down".into()), Some(2), cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(texts(&mut cx), vec!["2", "1", "3"], "cursor on row 2");

        cx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::Off,
            })
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-gutter-0").is_none(),
            "off again on the next draw"
        );
        assert!(
            (painted_tree_width(&mut cx) - base_width).abs() < 0.5,
            "and the tree cell gave the width back"
        );
    }

    /// I4, test 3 — the C1 regression. With more rows than the test
    /// viewport shows, the visible row *range* `TableState` computes is
    /// the same after a second snapshot delivery as after the first
    /// (there's nothing new to scroll to), so `visible_rows_changed` —
    /// the only other refill path besides `invalidate_cells` — never
    /// fires again for it. Before the C1 fix this left every cell in
    /// that unchanged range painting blank forever, the first time this
    /// happened onward.
    #[gpui::test]
    fn a_cell_still_has_text_after_a_second_snapshot_with_an_unchanged_visible_range(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // the initial "tree" view query, unused
        h.tile
            .update(&mut cx, |t, cx| t.command("view wide", cx).unwrap());
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(flat_snapshot(200, 0.0)));

        let window_before = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().cache.window());
        assert!(
            !window_before.is_empty(),
            "sanity: the first draw filled a real cache window"
        );
        assert!(
            window_before.end < 201,
            "sanity: fewer rows are visible than exist — {window_before:?} of 201"
        );

        // A second delivery for the *same* outstanding query (no new
        // requery) — the way a live tile receives a re-published
        // snapshot for the query it already has in flight.
        deliver(&h, &mut cx, p.tag, Ok(flat_snapshot(200, 1000.0)));

        let window_after = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().cache.window());
        assert!(
            !window_after.is_empty(),
            "the cache window must still cover real rows after the second \
             delivery, not collapse to empty just because nothing gave \
             `visible_rows_changed` a reason to fire again — got \
             {window_after:?} (was {window_before:?})"
        );

        let row_ix = window_after.start;
        let text = h.tile.read_with(&cx, |t, cx| {
            t.table()
                .read(cx)
                .delegate()
                .cache
                .get(row_ix, 1)
                .map(|c| c.text.to_string())
        });
        assert_eq!(
            text,
            Some("1,000.00".to_string()),
            "a visible cell must still have (fresh) text after a second \
             snapshot whose visible range didn't change — before the C1 \
             fix this cell painted blank forever once the range stopped \
             changing"
        );
        let selector: &'static str = Box::leak(format!("blotter-cell-{row_ix}-1").into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "the cell's element actually painted"
        );
    }

    /// Phase 4 §3.10, end to end at the tile level (shell-less: the
    /// barrier is opened by hand here exactly the way `ShellView::
    /// on_frame_changed` opens it in production — see `geode-shell`'s own
    /// `shell/tests/flip.rs` for that half). Two tiles share one frame:
    /// a scope change makes both requery, and while the barrier is open
    /// tile A's own outcome is staged rather than painted — only once
    /// tile B's outcome arrives too (emptying the barrier and bumping
    /// `flip`) do both tiles show the new snapshot, in the same notify
    /// pass. Repeated with B *failing* the second time: a failed outcome
    /// still counts as arrival, so A promotes on schedule and B keeps its
    /// last-good snapshot plus the error — one broken tile never holds
    /// the rest open.
    #[gpui::test]
    fn two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        assert_eq!(pa0.key, QueryKey(7));
        assert_eq!(pb0.key, QueryKey(8));

        // Baseline: both tiles land on `snapshot()`'s labels, no barrier
        // involved yet.
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let old_texts = vec!["".to_string(), "L1".into(), "L2".into()];
        assert_eq!(shown_texts(&h.a, &vcx), old_texts);
        assert_eq!(shown_texts(&h.b, &vcx), old_texts);

        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());

        // A scope change: this test drives the two steps directly, in
        // whichever order reaches "both queries in flight, no barrier
        // yet" — `set_text` + `notify` first (each tile's own frame
        // observer, set up in `new`, submits its query), `open_flip`
        // second. Production runs the OPPOSITE order within one notify
        // pass: `ShellView::on_frame_changed`'s frame observer is
        // registered (in `ShellView::new`) before any tile occupant's,
        // so it always opens the barrier FIRST, and a tile's own
        // `on_frame_changed`/requery follows in the same flush — see
        // `a_pinned_tile_arrives_from_on_frame_changed_without_
        // requerying` below, which drives that real order.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("A".into()));
            cx.notify();
        });
        let pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });

        // A's outcome arrives first: staged, not painted — the barrier
        // still wants B.
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa1.tag, Ok(snapshot2()));
        assert_eq!(
            shown_texts(&h.a, &vcx),
            old_texts,
            "A's own outcome landed but must wait for B"
        );
        assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));

        // B's outcome arrives: the barrier empties, `flip` bumps, and —
        // after the notify pass that schedules — both tiles show the new
        // snapshot in the same pass.
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        vcx.run_until_parked();
        assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()));
        let new_texts = vec!["".to_string(), "M1".into(), "M2".into()];
        assert_eq!(shown_texts(&h.a, &vcx), new_texts, "A promoted");
        assert_eq!(shown_texts(&h.b, &vcx), new_texts, "B applied directly");

        // Repeat, with B failing this time.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("B".into()));
            cx.notify();
        });
        let pa2 = next_query(&h.requests);
        let pb2 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa2.tag, Ok(snapshot()));
        assert_eq!(shown_texts(&h.a, &vcx), new_texts, "still staged");
        assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));

        deliver_to(
            &h.b,
            QueryKey(8),
            &mut vcx,
            pb2.tag,
            Err("binder error".into()),
        );
        vcx.run_until_parked();
        assert!(
            !frame.read_with(&vcx, |f, _| f.barrier_open()),
            "a failed outcome still counts as arrival"
        );
        assert_eq!(shown_texts(&h.a, &vcx), old_texts, "A promoted on schedule");
        assert_eq!(
            shown_texts(&h.b, &vcx),
            new_texts,
            "B keeps its last-good snapshot"
        );
        let b_error = h.b.read_with(&vcx, |t, _| t.error.clone());
        assert_eq!(b_error.as_deref(), Some("binder error"));
    }

    /// A pinned tile ignores a grouping-only change (§4.1: `follows_
    /// changed` is false for it) — but it still sits in the barrier's key
    /// set, and it must "arrive" on its own from `on_frame_changed`,
    /// never from `deliver` (it submits no new query at all), or it would
    /// hold its unpinned sibling's flip open until `FLIP_DEADLINE` for no
    /// reason (§3.10). The grouping mutation and `open_flip` are set up
    /// together, before the one `cx.notify()` that fans out to both
    /// tiles — the real order `ShellView::on_frame_changed` guarantees in
    /// production, since its own frame observer is registered (in
    /// `ShellView::new`) before any tile occupant's.
    #[gpui::test]
    fn a_pinned_tile_arrives_from_on_frame_changed_without_requerying(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));

        // Pin A to a fixed grouping.
        h.a.update(&mut vcx, |t, cx| t.command("group lhu", cx).unwrap());
        let pa_pin = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa_pin.tag, Ok(snapshot()));

        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
        frame.update(&mut vcx, |f, cx| {
            f.set_active_slot(Some(1));
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now());
            cx.notify();
        });

        // Only B (unpinned) requeries; A ignores the grouping change.
        let pb1 = next_query(&h.requests);
        assert!(
            h.requests.try_recv().is_err(),
            "A is pinned — it never requeries"
        );

        let v = frame.read_with(&vcx, |f, _| f.versions());
        assert!(
            frame.read_with(&vcx, |f, _| f.barrier_open()),
            "still waiting on B"
        );
        assert!(
            !frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(7), v)),
            "A already arrived on its own"
        );
        assert!(frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(8), v)));

        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert!(
            !frame.read_with(&vcx, |f, _| f.barrier_open()),
            "B's own arrival was enough — A never had to be waited on"
        );
    }

    /// Fix round 1, Finding 1: `staged` carried no version identity and
    /// survived `requery`. Trace reproduced here — tile B stages a
    /// snapshot for V1 while the barrier still awaits A; a second
    /// mutation lands within the 250ms window before A ever answers V1;
    /// `open_flip` replaces the barrier for V2; B's `on_frame_changed`
    /// sees `follows_changed(V2)` and requeries (bumping `tag`, setting
    /// `acted = V2`) while the stale V1 snapshot was still sitting in
    /// `staged`; the V2 barrier releases on the deadline before B's own
    /// V2 query lands; `flip` bumps and B's `on_frame_changed` must NOT
    /// promote the stale V1 payload under that bump.
    ///
    /// Three independent scenarios, because the fix's two halves are not
    /// redundant with each other and no single race tells them apart on
    /// its own (checked by hand, mutating each half separately against
    /// only the others — see the fix-round report for both console
    /// outputs): Part 1 (both tiles unpinned, a second *scope* change)
    /// is caught by either half alone — `requery`'s clear runs before
    /// the flip bumps, and `promote`'s version check would also reject
    /// the scope mismatch if it didn't. Part 2 pins B to a fixed
    /// grouping, so a *grouping-only* second mutation never makes B
    /// requery at all (`requery`'s clear never runs) — only `promote`'s
    /// version check stands between the stale V1 payload and the
    /// screen. Part 3 is the reverse: a `data`-only bump (which never
    /// opens or replaces a barrier, but `follows_changed` always
    /// compares `data`) forces B to requery while the *original* V1
    /// barrier — whose scope/grouping/as_of the data bump never
    /// touches — is still what releases on the deadline; `promote`'s
    /// version check alone would not catch this (it deliberately
    /// ignores `data`/`config`, same as `Frame::matches`), so only
    /// `requery`'s clear does.
    #[gpui::test]
    fn a_second_mutation_during_a_barrier_wait_clears_the_stale_staged_snapshot(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let baseline = shown_texts(&h.b, &vcx);

        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());

        // V1: a scope change opens a barrier over both keys.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("V1".into()));
            cx.notify();
        });
        let pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        let _ = pa1; // A's V1 query is left outstanding — never delivered.
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });

        // B's V1 outcome arrives first and stages — the barrier still
        // wants A.
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert_eq!(
            shown_texts(&h.b, &vcx),
            baseline,
            "V1 is staged, not painted"
        );
        assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));

        // V2 lands within the same window, before A ever answers V1 —
        // both tiles follow scope, so both requery again (A's own
        // now-doubly-stale V1 query is superseded the same way any
        // repeated scope edit supersedes an in-flight one — that part
        // isn't new here); `open_flip` replaces the barrier.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("V2".into()));
            cx.notify();
        });
        let pa2 = next_query(&h.requests);
        let pb2 = next_query(&h.requests);
        let _ = pa2;
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });

        // Nothing else ever arrives for the V2 barrier — the deadline
        // releases it, bumping `flip`.
        frame.update(&mut vcx, |f, cx| {
            assert!(f.sweep(Instant::now() + FLIP_DEADLINE + Duration::from_millis(1)));
            cx.notify();
        });

        // B must still show its pre-V1 rows — never the stale V1
        // payload — and its real V2 query is still outstanding.
        assert_eq!(
            shown_texts(&h.b, &vcx),
            baseline,
            "the stale V1 snapshot must never paint under the V2 flip"
        );
        assert!(
            h.requests.try_recv().is_err(),
            "the deadline releasing does not itself submit a query"
        );

        // B's real V2 outcome finally lands and paints.
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb2.tag, Ok(snapshot3()));
        assert_eq!(
            shown_texts(&h.b, &vcx),
            vec!["".to_string(), "N1".into(), "N2".into()],
            "the real V2 payload paints once it actually arrives"
        );

        // Part 2: B pinned to a fixed grouping — a grouping-only second
        // mutation never makes it requery, so only `promote`'s own
        // version check (not `requery`'s clear) can stop the stale V1
        // payload from painting.
        let (h2, mut vcx2) = open_two(cx);
        h2.a.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        h2.b.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        let qa0 = next_query(&h2.requests);
        let qb0 = next_query(&h2.requests);
        deliver_to(&h2.a, QueryKey(7), &mut vcx2, qa0.tag, Ok(snapshot()));
        deliver_to(&h2.b, QueryKey(8), &mut vcx2, qb0.tag, Ok(snapshot()));
        let baseline2 = shown_texts(&h2.b, &vcx2);

        h2.b.update(&mut vcx2, |t, cx| t.command("group lhu", cx).unwrap());
        let qb_pin = next_query(&h2.requests);
        deliver_to(&h2.b, QueryKey(8), &mut vcx2, qb_pin.tag, Ok(snapshot()));

        let frame2 = h2.a.read_with(&vcx2, |t, _| t.frame.clone());

        // V1: a scope change — pinned-to-grouping B still follows scope,
        // so it requeries and, once the barrier opens over it, stages.
        frame2.update(&mut vcx2, |f, cx| {
            f.set_text(Some("V1".into()));
            cx.notify();
        });
        let qa1 = next_query(&h2.requests);
        let qb1 = next_query(&h2.requests);
        let _ = qa1; // A's V1 query is left outstanding — never delivered.
        frame2.update(&mut vcx2, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h2.b, QueryKey(8), &mut vcx2, qb1.tag, Ok(snapshot2()));
        assert_eq!(
            shown_texts(&h2.b, &vcx2),
            baseline2,
            "V1 is staged, not painted"
        );
        assert!(frame2.read_with(&vcx2, |f, _| f.barrier_open()));

        // V2: a grouping-only change. B is pinned — it never requeries,
        // so `requery`'s clear never runs for it here. The mutation and a
        // fresh barrier over B's key alone are set up together, before
        // the one notify — B's own "does not follow" branch
        // (`a_pinned_tile_arrives_from_on_frame_changed_without_
        // requerying`'s own mechanism) answers it immediately, which,
        // since it is the barrier's only key, releases it and bumps
        // `flip` on the very next notify pass.
        frame2.update(&mut vcx2, |f, cx| {
            f.set_active_slot(Some(1));
            f.open_flip([QueryKey(8)], Instant::now());
            cx.notify();
        });
        vcx2.run_until_parked();

        // A (unpinned) follows the grouping change too — drain its own
        // fresh query, which has nothing to do with B's half of this
        // scenario.
        let qa2 = next_query(&h2.requests);
        assert_eq!(qa2.key, QueryKey(7));
        assert!(
            h2.requests.try_recv().is_err(),
            "B is pinned — a grouping-only change never requeries it"
        );

        assert_eq!(
            shown_texts(&h2.b, &vcx2),
            baseline2,
            "the stale V1 payload must never paint under the grouping-only flip"
        );

        // Part 3: a data-only bump forces B to requery while V1 is
        // staged (`follows_changed` always compares `data`), but the
        // *original* V1 barrier — whose scope/grouping/as_of the data
        // bump never touches — is what eventually releases on the
        // deadline. Only `requery`'s own clear stops the stale
        // (pre-data-bump) V1 snapshot from painting here.
        let (h3, mut vcx3) = open_two(cx);
        h3.a.update(&mut vcx3, |t, cx| t.set_visible(true, cx));
        h3.b.update(&mut vcx3, |t, cx| t.set_visible(true, cx));
        let ra0 = next_query(&h3.requests);
        let rb0 = next_query(&h3.requests);
        deliver_to(&h3.a, QueryKey(7), &mut vcx3, ra0.tag, Ok(snapshot()));
        deliver_to(&h3.b, QueryKey(8), &mut vcx3, rb0.tag, Ok(snapshot()));
        let baseline3 = shown_texts(&h3.b, &vcx3);

        let frame3 = h3.a.read_with(&vcx3, |t, _| t.frame.clone());

        // V1: a scope change opens a barrier over both keys.
        frame3.update(&mut vcx3, |f, cx| {
            f.set_text(Some("V1".into()));
            cx.notify();
        });
        let ra1 = next_query(&h3.requests);
        let rb1 = next_query(&h3.requests);
        let _ = ra1; // A's V1 query is left outstanding — the barrier
        // never releases on its own arrival in this scenario.
        frame3.update(&mut vcx3, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h3.b, QueryKey(8), &mut vcx3, rb1.tag, Ok(snapshot2()));
        assert_eq!(
            shown_texts(&h3.b, &vcx3),
            baseline3,
            "V1 is staged, not painted"
        );
        assert!(frame3.read_with(&vcx3, |f, _| f.barrier_open()));

        // A data bump: never opens or replaces the barrier, but B still
        // requeries because `follows_changed` always compares `data`.
        frame3.update(&mut vcx3, |f, cx| {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 1,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
        let ra_data = next_query(&h3.requests);
        let rb_data = next_query(&h3.requests);
        let _ = ra_data;

        // The original V1 barrier releases on the deadline — its own
        // scope/grouping/as_of were never touched by the data bump.
        frame3.update(&mut vcx3, |f, cx| {
            assert!(f.sweep(Instant::now() + FLIP_DEADLINE + Duration::from_millis(1)));
            cx.notify();
        });
        assert_eq!(
            shown_texts(&h3.b, &vcx3),
            baseline3,
            "the stale (pre-data-bump) V1 payload must never paint just \
             because scope/grouping/as_of still match — a fresher query \
             is already in flight for the data bump"
        );

        deliver_to(&h3.b, QueryKey(8), &mut vcx3, rb_data.tag, Ok(snapshot3()));
        assert_eq!(
            shown_texts(&h3.b, &vcx3),
            vec!["".to_string(), "N1".into(), "N2".into()],
            "the real, fresher payload paints once it actually arrives"
        );
    }

    /// Regression: `BlotterTile::completions` used to build its
    /// `Vocabulary` from the column plan/view alone (what's
    /// *displayed*), so `:group `/`:scope drop `/`:filter ` never
    /// offered a dimension the current view does not show —
    /// `model_code` here (`schema()`'s carried dimension, `grain =
    /// "instrument"`) is exactly that shape. `delta01`/`daily_trading_
    /// pnl` are measures and must never appear for `group`.
    #[gpui::test]
    fn completions_offer_dataset_dimensions_not_just_displayed_columns(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, vcx) = open(cx);
        let group = h
            .tile
            .read_with(&vcx, |t, cx| t.completions("group ", 6, cx));
        assert!(group.contains(&"book".to_string()));
        assert!(
            group.contains(&"model_code".to_string()),
            "a carried dimension the current view does not display: {group:?}"
        );
        assert!(
            !group.contains(&"delta01".to_string()),
            "a measure must not complete `group`: {group:?}"
        );
        let drop = h
            .tile
            .read_with(&vcx, |t, cx| t.completions("scope drop ", 11, cx));
        assert!(drop.contains(&"model_code".to_string()));
        assert!(!drop.contains(&"daily_trading_pnl".to_string()));
    }

    /// The shell cannot depend on `geode-blotter` (layering: shell never
    /// depends on a module), so `geode_shell::defaults` carries its own
    /// copy of these ids (and, since the final review, titles too) to
    /// reserve, ahead of `BlotterFactory::register_actions`, so
    /// `BUILTIN_KEYMAP`'s `blotter::*` bindings are never dropped as
    /// unregistered and the palette shows the same title either way.
    /// This pins both lists identical — ids via `BLOTTER_ACTIONS` (kept
    /// for the id-only comparison), titles via `BLOTTER_ACTION_DEFS`
    /// directly, since it was made `pub` for exactly this.
    #[test]
    fn the_shells_reserved_blotter_actions_match_ours() {
        let ours: Vec<&str> = ACTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ours, geode_shell::defaults::BLOTTER_ACTIONS.to_vec());
        assert_eq!(
            ACTIONS,
            geode_shell::defaults::BLOTTER_ACTION_DEFS,
            "titles must match too, not just ids — `ActionRegistry::register`'s \
             discarded `Err` on the shell's duplicate registration means the \
             shell's title, not the blotter's, is what actually reaches the palette"
        );
    }
}
