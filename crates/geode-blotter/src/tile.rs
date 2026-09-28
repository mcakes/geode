//! A blotter tile owns its local query overrides, table, and find state.
//! It observes frame changes, submits keyed queries through `DataHandle`,
//! rejects superseded outcomes, and coordinates snapshot display with the
//! frame's flip barrier. Header and footer rendering report query state,
//! freshness, selection summaries, and errors.

use crate::core::commands::{AsOfArg, Command, Vocabulary, completions, parse, parse_as_of};
use crate::core::find::FindState;
use crate::core::flatten::{SortOrder, SortSpec};
use crate::core::plan::ColumnKind;
use crate::core::yank::tsv;
use crate::delegate::{BlotterDelegate, CellPointer, ChevronClicked};
use geode_core::colour::NamedColours;
use geode_core::dimensions::DerivedDimensions;
use geode_core::grid::selection::{Lost, SelectKind, UNSUMMABLE_LEGEND};
use geode_core::groupings::GroupingSlots;
use geode_core::query::{AsOf, QueryKey, QueryOutcome};
use geode_core::schema::SchemaSpec;
use geode_core::scope::{Scope, parse_expr};
use geode_core::snapshot::Snapshot;
use geode_core::view::ViewSpec;
use geode_data::{DataHandle, QueryParams};
use geode_shell::actions::ActionId;
use geode_shell::colfit::{
    FitMetrics, FittedWidths, NOTHING_TO_FIT, SESSION_KEY, widths_from_record, widths_to_toml,
};
use geode_shell::fonts;
use geode_shell::frame::{FrameRef, FrameVersions, FrameView, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::linenumbers::{LineNumbers, UiSettings};
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::aggregates;
use geode_shell::shell::chip::{self, Tone};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_shell::vimfind::{FindDirection, FindStyle};
use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};
use geode_tile::notice::{self, Notice};
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, ElementId, Entity, IntoElement, SharedString, Window, div,
};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// After this long without a result the header shows an in-flight glyph.
const IN_FLIGHT_AFTER: Duration = Duration::from_millis(50);
/// Header and footer strip heights, in pixels at the design rem
/// (`geode_shell::shell::scale`): the strips follow the font size with
/// the text they hold. The market-data panel's header shares the 22.
const HEADER_HEIGHT: f32 = 22.0;
const FOOTER_HEIGHT: f32 = 20.0;

/// Default for `[app] blotter.stale_after`. The app supplies the configured
/// value through the factory's shared cell so open tiles follow reloads.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(15 * 60);

pub const ACTIONS: &[(&str, &str)] = &[
    ("blotter::expand", "Expand node"),
    ("blotter::collapse", "Collapse node"),
    ("blotter::toggle", "Toggle node"),
    ("blotter::expand_all", "Expand all"),
    ("blotter::collapse_all", "Collapse all"),
    ("blotter::visual_rows", "Select rows"),
    ("blotter::visual_block", "Select cells"),
    ("blotter::escape", "Clear selection / narrowing"),
    ("blotter::yank", "Yank selection as TSV"),
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

/// The tile's as-of override, independent of grouping and scope overrides.
/// A pinned tile queries at its own instant and ignores the frame's `as_of`
/// counter; `:asof clear` returns it to [`TileAsOf::Follow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileAsOf {
    /// Query at the frame's as-of; requery when it changes.
    Follow,
    /// Query at this instant regardless of the frame.
    Pinned(AsOf),
}

pub struct BlotterTile {
    tile: TileId,
    frame: FrameRef,
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    /// Named colour definitions shared through the factory and refreshed on
    /// `ConfigReloaded`. `apply` passes the current definitions to the delegate
    /// alongside its column plan so both reflect the same configuration.
    colours: Rc<RefCell<Arc<NamedColours>>>,
    /// Schema and derived dimensions used to validate tile-local filters.
    /// The factory shares and refreshes these on `ConfigReloaded`.
    schema: Rc<RefCell<SchemaSpec>>,
    dims: Rc<RefCell<DerivedDimensions>>,
    pub find_style: Rc<Cell<FindStyle>>,
    /// Configured freshness threshold, shared through the factory so reloads
    /// update open tiles without recreating them.
    pub stale_after: Rc<Cell<Duration>>,
    table: Entity<TableState<BlotterDelegate>>,
    view_name: String,
    pin: Pin,
    unscoped: bool,
    /// The `unscoped` pill's own tooltip selector (`"tip-blotter-
    /// unscoped-{id}"`), built once here since it depends only on the
    /// tile id, never per render.
    unscoped_tip_selector: SharedString,
    /// The as-of override; fresh tiles follow the frame.
    pub(crate) tile_as_of: TileAsOf,
    /// Pinned chip text and tooltip title. `set_tile_as_of` refreshes both;
    /// render also refreshes them when the clock or its date changes.
    /// Both are empty while following the frame.
    asof_chip: SharedString,
    asof_tip: SharedString,
    /// Date on `asof_chip_clock` when the chip text was built. Render checks
    /// this while pinned to an instant: a date-elided `AS OF HH:MM` label must
    /// regain its date after midnight. Unused while following the frame.
    pub(crate) asof_chip_date: chrono::NaiveDate,
    /// Clock used to build the chip text. A zone change can leave `today`
    /// unchanged while changing the displayed time, so render checks both
    /// this clock and `asof_chip_date` before reusing the cache.
    pub(crate) asof_chip_clock: geode_core::clock::Clock,
    /// `"tip-blotter-asof-{id}"`, built once.
    asof_tip_selector: SharedString,
    tile_scope: Scope,
    /// [`filter_summary`] of `tile_scope`, cached so the `filtered`
    /// pill's tooltip clones a `SharedString` at paint time rather than
    /// rebuilding the summary (a `Vec`/`String` allocation) every
    /// render — recomputed wherever `tile_scope` is assigned (`new`,
    /// `:filter`, `:filter clear`).
    filter_tip: SharedString,
    /// The `filtered` pill's own tooltip selector (`"tip-blotter-
    /// filtered-{id}"`), built once alongside `filter_tip`.
    filter_tip_selector: SharedString,
    publications: Vec<PublicationWatch>,
    last_grouping: Vec<String>,
    /// [`Self::title`]'s answer, cached so a stack-list row (which reads
    /// it every frame the list is open) never formats a `String`: kept
    /// in step with `view_name`/`last_grouping` at their one assignment
    /// site each (`requery`, right after `last_grouping` is set — a
    /// `view_name` change always runs through `requery` immediately
    /// after, so that one site covers both).
    title: SharedString,
    /// This tile's stack membership, shown in the header; `None` outside a stack.
    stack: Option<StackHandle>,
    delivered_at: Option<Instant>,
    visible: bool,
    /// Header notice. Dropped sorts and selections are warnings; query and
    /// configuration failures are danger.
    pub error: Option<Notice>,
    /// The refusal a restored view over a computed dataset opened with,
    /// pending the first delivery. That delivery is the fallback view's
    /// own snapshot, landing before the trader could read the header, so
    /// `apply` raises the notice again instead of letting it clear; the
    /// next delivery follows something the trader did and clears it.
    restored_view_refusal: Option<String>,
    find: Option<FindState>,
    /// This tile's view query under the flip barrier (see
    /// `geode_tile::following`), with the grouping each result was asked
    /// under. Promotion compares only counters this tile follows (`Followed`),
    /// including watched data and configuration; a tile-local requery clears
    /// the stage because it moves no frame counter.
    following: FollowingQuery<(Arc<Snapshot>, Vec<String>)>,
    /// The display column the last right press landed on, until
    /// `press_context` takes it for the shell's row menu.
    pressed: Option<usize>,
}

/// The frame counters a blotter's answer depends on, copied out of the tile
/// so the barrier helper can compare versions while the tile's query state
/// is mutably borrowed. Watched data and configuration are always followed;
/// scope unless unscoped, grouping unless pinned, as-of unless pinned.
#[derive(Debug, Clone, Copy)]
struct Followed {
    scope: bool,
    grouping: bool,
    as_of: bool,
}

impl Followed {
    fn differs(self, versions: FrameVersions, now: FrameVersions) -> bool {
        (self.scope && versions.scope != now.scope)
            || (self.grouping && versions.grouping != now.grouping)
            || (self.as_of && versions.as_of != now.as_of)
            || versions.data != now.data
            || versions.config != now.config
    }
}

impl BlotterTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: FrameRef,
        data: DataHandle,
        views: Rc<RefCell<Vec<ViewSpec>>>,
        colours: Rc<RefCell<Arc<NamedColours>>>,
        schema: Rc<RefCell<SchemaSpec>>,
        dims: Rc<RefCell<DerivedDimensions>>,
        find_style: Rc<Cell<FindStyle>>,
        stale_after: Rc<Cell<Duration>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let restored_name = restored.and_then(|t| t.get("view").and_then(|v| v.as_str()));
        // A restored view over a computed dataset exists but is a module's
        // to show: the fallback below applies as for a missing view, and
        // the tile opens with the refusal as its notice so the trader
        // learns why the record's view did not come back.
        let restored_view_computed = restored_name.and_then(|n| {
            let views = views.borrow();
            views
                .iter()
                .find(|v| v.name == n && !showable_in(&schema.borrow(), v))
                .map(computed_view_refusal)
        });
        let view_name = restored_name
            .map(str::to_string)
            .filter(|n| {
                views
                    .borrow()
                    .iter()
                    .any(|v| &v.name == n && showable_in(&schema.borrow(), v))
            })
            .or_else(|| {
                // Without a valid restored view, use the explicit default, then the
                // first configured view. `ViewSpec::from_doc` orders that fallback
                // by name when no default is set. Both arms skip a computed
                // view: the merged list holds the pricer's views beside the
                // blotter's, and one of those sorting first must not become
                // the view a fresh tile opens on and refuses.
                let views = views.borrow();
                let schema = schema.borrow();
                views
                    .iter()
                    .find(|v| v.is_default && showable_in(&schema, v))
                    .or_else(|| views.iter().find(|v| showable_in(&schema, v)))
                    .map(|v| v.name.clone())
            })
            .unwrap_or_default();
        let restored_view_kept = restored_name == Some(view_name.as_str());
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
        // A syntax error in restored `filter.expr` drops the entire tile filter,
        // including its text, and logs a warning. Restoration only parses the
        // expression here; it does not validate column names against the schema.
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
        // Restore `as_of` from "live" or an RFC 3339 instant. An absent or
        // malformed value follows the frame; malformed strings are logged.
        let tile_as_of = match restored
            .and_then(|t| t.get("as_of"))
            .and_then(|v| v.as_str())
        {
            None => TileAsOf::Follow,
            Some("live") => TileAsOf::Pinned(AsOf::Live),
            Some(text) => match chrono::DateTime::parse_from_rfc3339(text) {
                Ok(at) => TileAsOf::Pinned(AsOf::At(at.with_timezone(&chrono::Utc))),
                Err(e) => {
                    tracing::warn!(
                        target: "geode::session",
                        "tile {}: restored as_of '{text}' is not RFC 3339 ({e}) — following the frame",
                        tile.0
                    );
                    TileAsOf::Follow
                }
            },
        };
        // Independently hosted tiles may have no `AppClock`; use the machine
        // clock as a fallback.
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        let (asof_chip, asof_tip) = Self::asof_chip_strings(&tile_as_of, clock);
        let asof_chip_date = clock.today(chrono::Utc::now());
        let asof_chip_clock = clock;
        let asof_tip_selector: SharedString = format!("tip-blotter-asof-{}", tile.0).into();
        let unscoped_tip_selector: SharedString = format!("tip-blotter-unscoped-{}", tile.0).into();
        let filter_tip_selector: SharedString = format!("tip-blotter-filtered-{}", tile.0).into();
        let filter_tip: SharedString = filter_summary(&tile_scope).into();

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
            // A missing or garbled record is an empty map, never a refusal.
            // Widths fitted for a view the record names but that no longer
            // exists belong to other columns: the fallback view starts
            // without them.
            if restored_view_kept {
                delegate.fitted = widths_from_record(restored);
            }
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .loop_selection(false)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
        });
        cx.subscribe(&table, |this, _, event: &TableEvent, cx| match event {
            // Only the cursor row moves here: a live selection is the
            // mouse doors' business (`pointer`, below) and a plain
            // `SelectRow` — the table's own reaction to any press,
            // selection gesture or not — must never clear one out from
            // under a shift+click or drag that just started it.
            TableEvent::SelectRow(row) => {
                // `set_selected_row` (`sync_cursor`, below) re-emits this
                // same event unconditionally, row unchanged or not — and
                // a live snapshot can call it too, on every tick, with
                // the cursor's own row. Returning early when the row is
                // already where the cursor is breaks that echo without a
                // guard elsewhere, and keeps a live tick from scrolling a
                // sideways-scrolled view back to the cursor's column.
                let moved = this.with_delegate(cx, |d| {
                    let len = d.shown.len();
                    if (*row).min(len.saturating_sub(1)) == d.cursor.row {
                        return false;
                    }
                    d.cursor.to_row(*row, len);
                    true
                });
                if moved {
                    this.sync_cursor(cx);
                }
            }
            // A double-click anywhere on a row is `space` on it. The
            // table selects the row before emitting this, so the cursor
            // is already there; `toggle_row` moves it again regardless.
            TableEvent::DoubleClickedRow(row) => this.toggle_row(*row, cx),
            _ => {}
        })
        .detach();
        cx.subscribe(&table, |this, _, event: &ChevronClicked, cx| {
            this.toggle_row(event.0, cx);
        })
        .detach();
        cx.subscribe(&table, |this, _, event: &CellPointer, cx| {
            this.pointer(*event, cx)
        })
        .detach();
        cx.observe(frame.entity(), |this, _, cx| this.on_frame_changed(cx))
            .detach();
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();
        // Clock changes repaint freshness readouts and the pinned as-of chip.
        // Render reads the clock and invalidates the chip cache when needed.
        cx.observe_global::<geode_shell::clock::AppClock>(|_this, cx| cx.notify())
            .detach();

        let title = Self::compute_title(&view_name, &[]);

        BlotterTile {
            tile,
            frame,
            data,
            views,
            colours,
            schema,
            dims,
            find_style,
            stale_after,
            table,
            view_name,
            pin,
            unscoped,
            unscoped_tip_selector,
            tile_as_of,
            asof_chip,
            asof_tip,
            asof_chip_date,
            asof_chip_clock,
            asof_tip_selector,
            tile_scope,
            filter_tip,
            filter_tip_selector,
            publications: Vec::new(),
            last_grouping: Vec::new(),
            title,
            stack: None,
            delivered_at: None,
            visible: false,
            error: restored_view_computed.clone().map(Notice::danger),
            restored_view_refusal: restored_view_computed,
            find: None,
            following: FollowingQuery::new(),
            pressed: None,
        }
    }

    pub fn table(&self) -> &Entity<TableState<BlotterDelegate>> {
        &self.table
    }

    /// The dimension context at the cursor row (see
    /// [`BlotterDelegate::dimension_context`]): its single-valued columns,
    /// plus the selection's rows while the cursor is inside it. `None`
    /// before the first snapshot.
    pub fn dimension_context(&self, cx: &App) -> Option<geode_core::context::DimensionContext> {
        self.table.read(cx).delegate().dimension_context()
    }

    /// The row a right press just landed on, for the shell's row menu,
    /// leading with the pressed column when it is a dimension the row
    /// carries (never the tree column or a measure). Takes the press: a
    /// second call answers `None`.
    pub fn press_context(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<geode_core::context::DimensionContext> {
        let col = self.pressed.take()?;
        let d = self.table.read(cx).delegate();
        let mut ctx = d.dimension_context()?;
        ctx.first = d
            .plan
            .as_ref()
            .and_then(|p| p.columns.get(col))
            .filter(|c| c.kind == ColumnKind::Dimension && ctx.get(&c.name).is_some())
            .map(|c| c.name.clone());
        Some(ctx)
    }

    /// The presented columns and the cursor's column, for the shell's
    /// edit-column actions. `None` until a plan exists or when the view is
    /// no longer configured.
    pub fn tile_columns(&self, cx: &App) -> Option<geode_core::tile_columns::TileColumns> {
        let view = self.view()?;
        let d = self.table.read(cx).delegate();
        d.plan
            .as_ref()
            .map(|plan| plan.tile_columns(&view, d.cursor.col))
    }

    pub fn last_query(&self) -> Option<(u64, Vec<String>)> {
        let tag = self.following.tag();
        (tag > 0).then(|| (tag, self.last_grouping.clone()))
    }

    fn view(&self) -> Option<ViewSpec> {
        self.views
            .borrow()
            .iter()
            .find(|v| v.name == self.view_name)
            .cloned()
    }

    /// Validate a tile-local `:filter` against its dataset and derived dimensions.
    /// Unknown columns and unsupported operators are reported before the scope
    /// changes. The shell's expression dialogs perform the corresponding check
    /// for frame expressions; this method does not revalidate restored frames.
    /// Saved scopes are checked separately by `Scope::validate` when loaded.
    ///
    /// The parser refuses `:scope` because tile commands cannot change the frame.
    /// An unresolved view or dataset returns `Ok(())` here; query setup reports
    /// the missing configuration.
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

    /// Update the tile filter and its tooltip together. Every filter command
    /// uses this method so the hover summary always describes the active scope.
    fn set_tile_scope(&mut self, scope: Scope) {
        self.filter_tip = filter_summary(&scope).into();
        self.tile_scope = scope;
    }

    /// Chip text and tooltip title for the tile's as-of override.
    fn asof_chip_strings(
        as_of: &TileAsOf,
        clock: geode_core::clock::Clock,
    ) -> (SharedString, SharedString) {
        match as_of {
            TileAsOf::Follow => (SharedString::default(), SharedString::default()),
            TileAsOf::Pinned(AsOf::Live) => ("LIVE".into(), "Pinned to live".into()),
            TileAsOf::Pinned(AsOf::At(at)) => (
                pinned_chip_text(*at, chrono::Utc::now(), clock).into(),
                format!(
                    "Pinned to {}",
                    clock.local(*at).format("%Y-%m-%d %H:%M:%S %Z")
                )
                .into(),
            ),
        }
    }

    /// Refresh the chip text, tooltip, date, and clock as one cache entry.
    /// Called on as-of edits and from render after a date or zone change;
    /// checking the clock also catches zone changes within the same date.
    fn refresh_asof_chip(&mut self, clock: geode_core::clock::Clock) {
        let (chip, tip) = Self::asof_chip_strings(&self.tile_as_of, clock);
        self.asof_chip = chip;
        self.asof_tip = tip;
        self.asof_chip_date = clock.today(chrono::Utc::now());
        self.asof_chip_clock = clock;
    }

    /// Change the as-of override and refresh its chip cache.
    /// Returns whether the override changed and needs a requery.
    fn set_tile_as_of(&mut self, next: TileAsOf, clock: geode_core::clock::Clock) -> bool {
        if self.tile_as_of == next {
            return false;
        }
        self.tile_as_of = next;
        self.refresh_asof_chip(clock);
        true
    }

    fn grouping(&self, frame: &FrameView<'_>, view: &ViewSpec) -> Vec<String> {
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

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame.read(cx).versions_for(&self.publications)
    }

    fn watch_view(&mut self, view: &ViewSpec, cx: &mut Context<Self>) {
        let datasets = || {
            std::iter::once(view.dataset.as_str())
                .chain(view.joins.iter().map(|join| join.dataset.as_str()))
        };
        if self.publications.len() == 1 + view.joins.len()
            && self
                .publications
                .iter()
                .zip(datasets())
                .all(|(watch, dataset)| watch.matches(dataset, None))
        {
            return;
        }
        self.publications = self.frame.update(cx, |frame, _| {
            datasets()
                .map(|dataset| frame.watch_publications(dataset, None))
                .collect()
        });
    }

    fn followed(&self) -> Followed {
        Followed {
            scope: !self.unscoped,
            grouping: self.pin == Pin::None,
            as_of: matches!(self.tile_as_of, TileAsOf::Follow),
        }
    }

    /// Compare the counters this tile follows. Requery and staged-snapshot
    /// promotion share this comparison so they agree about which changes
    /// invalidate an answer.
    fn differs_on_followed(&self, versions: FrameVersions, now: FrameVersions) -> bool {
        self.followed().differs(versions, now)
    }

    fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        // Attempt promotion on every flip, including while hidden, so hiding
        // between staging and release does not leave a valid answer waiting.
        // `flip` itself is never a requery input.
        let now = self.versions(cx);
        let followed = self.followed();
        let differs = move |a, b| followed.differs(a, b);
        let promoted = self.following.on_flip(now, differs);
        if let Promotion::Apply((snapshot, grouping)) = promoted {
            self.apply(snapshot, grouping, cx);
        }
        if !self.visible {
            return;
        }
        if self.following.follows_changed(now, differs) {
            self.requery(cx);
        } else {
            // A tile can belong to the barrier without following its change;
            // unless its own query for this flip identity is still out, it
            // answers now so its siblings are not held.
            let key = QueryKey(self.tile.0);
            self.following
                .self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
        }
    }

    /// Apply a query snapshot with the grouping it was requested under.
    /// The delegate rebuilds its display state; refreshing the table updates
    /// columns and selected row, and delivery time starts the paint timer.
    /// The immutable snapshot is shared directly with the delegate.
    fn apply(&mut self, snapshot: Arc<Snapshot>, grouping: Vec<String>, cx: &mut Context<Self>) {
        // Raised before the notices below so a fresher one about this very
        // snapshot (a dropped sort or selection) still wins the one slot.
        if let Some(refusal) = self.restored_view_refusal.take() {
            self.error = Some(Notice::danger(refusal));
        }
        if let Some(view) = self.view() {
            // The plan is (re)built from `view` here, so the definitions
            // its `Colour::Named` columns resolve against are refreshed
            // in the same breath — a reloaded `colours.toml` reaches the
            // paint on the requery every applied reload already triggers
            // (`Frame::note_config_reloaded`), never a frame behind it.
            let colours = Arc::clone(&self.colours.borrow());
            let dropped_sort = self.table.update(cx, |t, cx| {
                t.delegate_mut().set_colours(colours);
                // `refresh` re-prepares the column groups from `column()` (the
                // `on_ui_settings` gotcha), so a plan whose labels or widths
                // changed reaches the header through it — no separate header
                // relayout is needed, and whether the plan was replaced is
                // nothing this door has to act on.
                t.delegate_mut().apply_snapshot(snapshot, &view, &grouping);
                t.refresh(cx);
                let row = t.delegate().cursor.row;
                t.set_selected_row(row, cx);
                // Taken, not read: a rebuild that carried no drop must not
                // leave a stale name behind for the next one to repeat.
                t.delegate_mut().dropped_sort.take()
            });
            if let Some(name) = dropped_sort {
                self.error = Some(Notice::warning(format!(
                    "sort on '{name}' dropped: the column is no longer in this view"
                )));
            }
            self.take_selection_notice(cx);
        }
        self.delivered_at = Some(Instant::now());
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        // A new question supersedes staged work even when frame versions are
        // unchanged, as with tile-local filters and grouping overrides.
        // Every path clears it in `begin`, including the two that ask
        // nothing.
        let Some(view) = self.view() else {
            // A view the configuration no longer defines is this tile's
            // error, never a query, and one broken tile never holds the rest
            // open: answer the barrier now, as a refused submission does.
            // `begin` supersedes any query still out for the old view, so
            // its late outcome cannot clear this error. `acted` stays set:
            // the reload that defines the view again bumps the config
            // version, which is the retry.
            self.error = Some(Notice::danger(format!(
                "view '{}' is not configured",
                self.view_name
            )));
            // No query goes out, so no delivery will ever consume a
            // refusal pending from the record; the unconfigured view is
            // the tile's whole story now.
            self.restored_view_refusal = None;
            let versions = self.versions(cx);
            self.following.begin(versions, Instant::now());
            let key = QueryKey(self.tile.0);
            self.following.submitted(
                false,
                Unanswered::KeepActed,
                &mut FrameDoor::new(&self.frame, cx),
                key,
            );
            cx.notify();
            return;
        };
        self.watch_view(&view, cx);
        let (grouping, scope, as_of, versions) = {
            let frame = self.frame.read(cx);
            let grouping = self.grouping(&frame, &view);
            // A tile scope comes from `:filter` and never names an
            // expression, so the unscoped branch needs no resolution; the
            // compiler refuses one that somehow does.
            let scope = if self.unscoped {
                Ok(self.tile_scope.clone())
            } else {
                frame.effective_scope(&self.tile_scope)
            };
            let as_of = match &self.tile_as_of {
                TileAsOf::Follow => frame.as_of().clone(),
                TileAsOf::Pinned(pinned) => pinned.clone(),
            };
            (
                grouping,
                scope,
                as_of,
                frame.versions_for(&self.publications),
            )
        };
        let scope = match scope {
            Ok(scope) => scope,
            Err(message) => {
                // An unresolved named expression is this tile's error, never
                // a query: dropping the name would widen the scope into
                // plausible wrong totals. Nothing will arrive for these
                // versions, so acknowledge the barrier now or every other
                // following tile waits out `FLIP_DEADLINE`. `acted` stays
                // set: redefining the name bumps the config version, which
                // is the retry.
                self.error = Some(Notice::danger(message));
                // Supersede any query still in flight: its outcome is for the
                // previous scope and must not paint over this error.
                self.following.begin(versions, Instant::now());
                let key = QueryKey(self.tile.0);
                self.following.submitted(
                    false,
                    Unanswered::KeepActed,
                    &mut FrameDoor::new(&self.frame, cx),
                    key,
                );
                cx.notify();
                return;
            }
        };
        let max_depth = self.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            d.expansion.prune_to(grouping.len());
            d.depth_bound(grouping.len()).max(1)
        });
        let submitted = Instant::now();
        let tag = self.following.begin(versions, submitted);
        self.last_grouping = grouping.clone();
        self.title = Self::compute_title(&self.view_name, &self.last_grouping);
        let key = QueryKey(self.tile.0);
        let queued = self.data.query(QueryParams {
            key,
            tag,
            submitted,
            view: self.view_name.clone(),
            grouping: Some(grouping),
            scope,
            as_of,
            max_depth,
        });
        if let Err(refusal) = &queued {
            // A stopped refusal retries on the next frame change too: each
            // attempt costs nothing and re-reports the same kind. The last
            // snapshot stays.
            self.error = Some(Notice::danger(format!("query refused: {refusal}")));
        }
        self.following.submitted(
            queued.is_ok(),
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        // Repaint once the in-flight affordance is due, if still waiting.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(IN_FLIGHT_AFTER + Duration::from_millis(10))
                .await;
            let _ = this.update(cx, |t, cx| {
                if t.following.in_flight() {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let now = self.versions(cx);
        let followed = self.followed();
        let result = outcome
            .snapshot
            .map(|snapshot| (snapshot, self.last_grouping.clone()));
        let key = QueryKey(self.tile.0);
        let delivered = self.following.deliver(
            outcome.tag,
            result,
            now,
            move |a, b| followed.differs(a, b),
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        if let Delivered::Stale = delivered {
            return; // stale: a newer request is out
        }
        let micros = outcome.submitted.elapsed().as_micros() as u64;
        self.frame
            .update(cx, |f, _| f.requery.record_submit_to_snapshot(micros));
        match delivered {
            Delivered::Stale => {}
            Delivered::Apply((snapshot, grouping)) => {
                self.error = None;
                self.apply(snapshot, grouping, cx);
            }
            Delivered::Held => self.error = None,
            // Asked under a scope, grouping, as-of, watched publication or
            // configuration this tile has since moved past while hidden:
            // neither painted nor a verdict on the current question, so the
            // header keeps what it says. The reshow asks again.
            Delivered::Superseded => {}
            // The last good snapshot stays; the failure has already arrived.
            Delivered::Failed(e) => self.error = Some(Notice::danger(e)),
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            let now = self.versions(cx);
            if self
                .following
                .follows_changed(now, |a, b| self.differs_on_followed(a, b))
            {
                self.requery(cx);
            }
        }
    }

    /// The shell is removing this tile: cancel its view query by key and
    /// answer any barrier still waiting on it. Hiding cancels nothing. Runs
    /// inside the shell's occupant reconciliation, so it updates only the
    /// frame and the data handle.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.tile.0);
        self.data.cancel(key);
        self.following
            .close(&mut FrameDoor::new(&self.frame, cx), key);
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn compute_title(view_name: &str, grouping: &[String]) -> SharedString {
        format!("{} · {}", view_name, GroupingSlots::label_of(grouping)).into()
    }

    /// A view the blotter can query: not over a computed dataset.
    fn showable(&self, view: &ViewSpec) -> bool {
        showable_in(&self.schema.borrow(), view)
    }

    pub fn key_context(&self, cx: &App) -> KeyContext {
        let d = self.table.read(cx).delegate();
        // `grid` is the flag the shell's shared motion bindings are written
        // under: without it no motion key reaches the blotter.
        let mut ctx = KeyContext::new("blotter").grid().pair(
            "mode",
            if d.selection.is_some() {
                "visual"
            } else {
                "normal"
            },
        );
        if let Some(s) = &d.selection {
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

    fn sync_cursor(&mut self, cx: &mut Context<Self>) {
        self.with_delegate(cx, |d| d.refresh_selection());
        self.table.update(cx, |t, cx| {
            let (row, col) = (t.delegate().cursor.row, t.delegate().cursor.col);
            // Always clear `right_clicked_row` as well as setting the cursor row.
            // The `SelectRow` handler returns early for an unchanged row, preventing
            // this call's unconditional event from recursing.
            t.set_selected_row(row, cx);
            t.scroll_to_row(row, cx);
            t.scroll_to_col(col, cx);
        });
        self.take_selection_notice(cx);
        cx.notify();
    }

    /// Handle mouse selection through the delegate state used by keyboard actions.
    /// A plain press clears the selection. A shift press or drag starts one
    /// at the current cursor if none is active, then moves the cursor to the
    /// pointer target. Extending a live selection preserves its kind and anchor.
    fn pointer(&mut self, event: CellPointer, cx: &mut Context<Self>) {
        if let CellPointer::Context { row, col } = event {
            self.context_press(row, col, cx);
            return;
        }
        let kind_for = |gutter: bool| {
            if gutter {
                SelectKind::Rows
            } else {
                SelectKind::Block
            }
        };
        self.with_delegate(cx, |d| {
            let (row, col, start) = match event {
                CellPointer::Press {
                    row,
                    col,
                    shift: false,
                    ..
                } => {
                    d.selection = None;
                    (row, col, None)
                }
                CellPointer::Press {
                    row,
                    col,
                    shift: true,
                    gutter,
                } => (row, col, Some(kind_for(gutter))),
                CellPointer::Drag { row, col, gutter } => {
                    if (row, col) == (d.cursor.row, d.cursor.col) {
                        return;
                    }
                    (row, col, Some(kind_for(gutter)))
                }
                CellPointer::Context { .. } => return, // `context_press`, above
            };
            if let Some(kind) = start
                && d.selection.is_none()
            {
                d.start_selection(kind);
            }
            let cols = d.plan.as_ref().map_or(0, |p| p.columns.len());
            let len = d.shown.len();
            d.cursor.to_row(row, len);
            d.cursor.col = col.min(cols.saturating_sub(1));
        });
        self.sync_cursor(cx);
    }

    /// A right press on display cell (`row`, `col`), ahead of the shell's
    /// row menu. Inside a live row (`V`) selection nothing moves, so the
    /// selected rows ride along; anywhere else it is a plain press: the
    /// selection clears and the cursor moves to the cell. Either way the
    /// column is recorded for [`Self::press_context`].
    fn context_press(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let inside = self
            .table
            .read(cx)
            .delegate()
            .resolved
            .as_ref()
            .is_some_and(|r| r.kind == SelectKind::Rows && r.rows.contains(&row));
        if !inside {
            self.pointer(
                CellPointer::Press {
                    row,
                    col,
                    shift: false,
                    gutter: false,
                },
                cx,
            );
        } else {
            // The table's own right-press highlight; `sync_cursor` clears
            // it on the other branch.
            self.table
                .update(cx, |t, cx| t.set_right_clicked_row(None, cx));
        }
        self.pressed = Some(col);
        cx.notify();
    }

    /// `zo`/`zc`/`za`/`space` on the cursor row, `n` times — and the one
    /// path every mouse toggle (a row double-click, a chevron click) goes
    /// through too, so the keyboard and the mouse can never disagree
    /// about what opening a node entails: reflatten, refresh the table,
    /// resync the cursor, and requery one level deeper when the node
    /// opened past what the snapshot materialised.
    fn expand_at_cursor(&mut self, open: Option<bool>, n: u32, cx: &mut Context<Self>) {
        let grouping_len = self.last_grouping.len();
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

    /// A mouse toggle on the *shown* row `row`: the cursor is moved there
    /// explicitly rather than trusting that the table's own `SelectRow`
    /// arrived first, then the row toggles as `space` would.
    fn toggle_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.with_delegate(cx, |d| d.cursor.to_row(row, d.shown.len()));
        self.expand_at_cursor(None, 1, cx);
    }

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(m) = geode_tile::motion::parse(action, count) {
            self.with_delegate(cx, |d| {
                let len = d.shown.len();
                let cols = d.plan.as_ref().map_or(0, |p| p.columns.len());
                // A bare j/k wraps outside a selection only: wrapping past
                // the anchor would silently invert it.
                let selecting = d.selection.is_some();
                d.cursor.apply(m, len, cols, selecting);
            });
            self.sync_cursor(cx);
            return true;
        }
        let Some(name) = action.0.strip_prefix("blotter::") else {
            return false;
        };
        match name {
            "expand" | "collapse" | "toggle" => {
                let open = match name {
                    "expand" => Some(true),
                    "collapse" => Some(false),
                    _ => None,
                };
                self.expand_at_cursor(open, count.unwrap_or(1).max(1), cx);
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
            "visual_rows" | "visual_block" => {
                let kind = if name == "visual_rows" {
                    SelectKind::Rows
                } else {
                    SelectKind::Block
                };
                self.with_delegate(cx, |d| d.start_selection(kind));
                self.table.update(cx, |_, cx| cx.notify());
                cx.notify();
            }
            "escape" => {
                if self.with_delegate(cx, |d| d.selection.is_some()) {
                    self.with_delegate(cx, |d| d.clear_selection());
                } else {
                    self.with_delegate(cx, |d| {
                        if d.narrowed.is_some() {
                            d.set_narrowed(None);
                        }
                    });
                    self.find = None;
                    self.table.update(cx, |t, cx| t.refresh(cx));
                }
            }
            "yank" => {
                let text = self.with_delegate(cx, |d| {
                    let (Some(snapshot), Some(plan)) = (&d.snapshot, &d.plan) else {
                        return None;
                    };
                    let (rows, cols) = match &d.resolved {
                        Some(r) => (r.rows.clone(), r.cols.clone()),
                        None => (d.cursor.row..d.cursor.row + 1, 0..plan.columns.len()),
                    };
                    let out = tsv(snapshot, plan, &d.shown, rows, cols);
                    d.clear_selection();
                    Some(out)
                });
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                cx.notify();
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
                    let Some(name) = d
                        .plan
                        .as_ref()
                        .and_then(|p| p.columns.get(col))
                        .map(|c| c.name.clone())
                    else {
                        return;
                    };
                    let measure = d.is_measure(col);
                    let current = d
                        .sort
                        .as_ref()
                        .filter(|s| s.column == name)
                        .map(|s| s.order);
                    let next = SortOrder::cycle(current, absolute, measure);
                    if next == current {
                        return;
                    }
                    d.sort = next.map(|order| SortSpec {
                        column: name,
                        order,
                    });
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
        self.take_selection_notice(cx);
        cx.notify();
        true
    }

    /// The tile's one-shot notice for `refresh_selection` clearing a
    /// selection whose anchor row, or block anchor column, is no longer
    /// shown — `selection_lost` is taken, not read, so the same loss is
    /// never reported twice. The anchor is whichever end the selection
    /// started from, not necessarily the range's first row.
    fn take_selection_notice(&mut self, cx: &mut Context<Self>) {
        if let Some(lost) = self.with_delegate(cx, |d| d.selection_lost.take()) {
            let text = match lost {
                Lost::Row => "selection cleared: anchor row no longer shown",
                Lost::Column => "selection cleared: anchor column no longer shown",
            };
            self.error = Some(Notice::warning(text));
        }
    }

    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match parse(line)? {
            Command::Autosize { reset } => self
                .autosize_columns(reset, window, cx)
                .map_err(str::to_string)?,
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
                self.set_tile_scope(scope);
                self.requery(cx);
            }
            Command::FilterText(words) => {
                let mut scope = self.tile_scope.clone();
                scope.text = (!words.trim().is_empty()).then_some(words);
                self.set_tile_scope(scope);
                self.requery(cx);
            }
            Command::FilterClear => {
                self.set_tile_scope(Scope::default());
                self.requery(cx);
            }
            Command::Refused(message) => return Err(message.to_string()),
            Command::AsOf(arg) => {
                // Independently hosted tiles may have no `AppClock`; use the
                // machine clock as a fallback.
                let clock = cx
                    .try_global::<geode_shell::clock::AppClock>()
                    .map(|c| c.0)
                    .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
                let next = match arg {
                    AsOfArg::At(text) => {
                        TileAsOf::Pinned(AsOf::At(parse_as_of(&text, chrono::Utc::now(), &clock)?))
                    }
                    AsOfArg::Live => TileAsOf::Pinned(AsOf::Live),
                    AsOfArg::Clear => TileAsOf::Follow,
                };
                if self.set_tile_as_of(next, clock) {
                    self.requery(cx);
                }
            }
            Command::View(name) => {
                {
                    let views = self.views.borrow();
                    let Some(view) = views.iter().find(|v| v.name == name) else {
                        return Err(format!("no view named '{name}'"));
                    };
                    if !self.showable(view) {
                        return Err(computed_view_refusal(view));
                    }
                }
                // A refusal still pending from the session record is about
                // the record's view, not this one: the new view's first
                // snapshot must not raise it.
                self.restored_view_refusal = None;
                self.view_name = name;
                // Another view is another column set: its fitted widths
                // would name columns that may mean something else there.
                self.with_delegate(cx, |d| {
                    d.plan = None;
                    d.fitted.clear();
                });
                self.requery(cx);
            }
            Command::Sort { column, order } => {
                let found = self.with_delegate(cx, |d| {
                    let col = d.plan.as_ref()?.position_of(&column)?;
                    // A text column has no magnitude: `abs` on it is its
                    // signed direction, in the state as on the screen.
                    let order = order.on_column(d.is_measure(col));
                    d.sort = Some(SortSpec {
                        column: column.clone(),
                        order,
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

    /// Fit every column to its header and the loaded rows (`reset`:
    /// drop the fitted widths), then refresh so the table re-reads
    /// `column()`. The one route behind both `:autosize` and the shell's
    /// `tile::autosize_columns`. Measures on the UI thread at the window's
    /// current rem, never in render; see `BlotterDelegate::fit_columns`
    /// for which rows count.
    ///
    /// With nothing loaded (no snapshot yet, or no rows) a fit refuses with
    /// [`NOTHING_TO_FIT`] and the widths already held stay; a reset always
    /// runs.
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
                .fit_columns(&metrics, cx)
                .ok_or(NOTHING_TO_FIT)?
        };
        self.table.update(cx, |t, cx| {
            t.delegate_mut().fitted = fitted;
            t.refresh(cx);
        });
        cx.notify();
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        // Sort completion uses displayed columns. Before the first snapshot,
        // fall back to the view's declarations so completion works immediately.
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
        // Grouping and filter completions use every groupable dataset column,
        // including dimensions absent from the view, plus derived dimensions.
        // Sort completion uses the displayed `columns` assembled above.
        let mut dimensions: Vec<String> = match self.view() {
            Some(v) => {
                let schema = self.schema.borrow();
                match schema.dataset(&v.dataset) {
                    // The compiler's own grouping vocabulary, spelled out
                    // once in `DatasetSpec::groupable_columns` (the
                    // Groupings dialog reads the same method).
                    Some(ds) => ds
                        .groupable_columns()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
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
        let views = self
            .views
            .borrow()
            .iter()
            .filter(|v| self.showable(v))
            .map(|v| v.name.clone())
            .collect();
        completions(
            line,
            cursor,
            &Vocabulary {
                columns,
                dimensions,
                views,
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
                // Fzf matches against the full `visible` row list on every change.
                // Reusing `shown` would misinterpret positions after narrowing and
                // prevent a shortened query from restoring excluded matches. Vim
                // uses `shown_texts` because its hits are cursor positions.
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

    pub fn serialize(&self, cx: &App) -> toml::Table {
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
        if let Some(v) = self.as_of_record() {
            t.insert("as_of".into(), v);
        }
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
        if let Some(w) = widths_to_toml(&self.table.read(cx).delegate().fitted) {
            t.insert(SESSION_KEY.into(), w);
        }
        t
    }

    /// Session `as_of`: omitted while following, `"live"` for a live pin,
    /// or the pinned instant in RFC 3339 (UTC).
    fn as_of_record(&self) -> Option<toml::Value> {
        match &self.tile_as_of {
            TileAsOf::Follow => None,
            TileAsOf::Pinned(AsOf::Live) => Some(toml::Value::String("live".into())),
            TileAsOf::Pinned(AsOf::At(at)) => Some(toml::Value::String(
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )),
        }
    }

    /// Whether a parseable freshness timestamp is older than `stale_after`.
    /// Missing or malformed timestamps are not marked stale; future timestamps
    /// have zero age.
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

    /// The tile's current notice or error text, as the header shows it.
    #[cfg(test)]
    pub fn error_text(&self) -> Option<String> {
        self.error.as_ref().map(|e| e.text().to_string())
    }

    /// Freshness readouts using render's clock lookup, formatting, and
    /// `as_of` ordering. Tests can inspect the text without a pixel reader.
    #[cfg(test)]
    pub(crate) fn freshness_texts(&self, cx: &App) -> Vec<String> {
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        let Some(snapshot) = self.table.read(cx).delegate().snapshot.clone() else {
            return Vec::new();
        };
        let p = snapshot.provenance();
        let mut datasets: Vec<_> = p.datasets.iter().collect();
        datasets.sort_by(|a, b| a.as_of.cmp(&b.as_of));
        datasets
            .into_iter()
            .map(|f| match &f.as_of {
                Some(t) => format!("{} {}", f.dataset, short_time(t, clock)),
                None => format!("{} \u{2014}", f.dataset),
            })
            .collect()
    }
}

/// A view the blotter can query: not over a computed dataset. Such a
/// dataset has no tables and a module answers for its views, so the
/// blotter refuses them itself rather than showing the trader a query
/// refusal for a view that was never its to show. A view over a dataset
/// the schema does not name is left to the query path's own diagnostic.
fn showable_in(schema: &SchemaSpec, view: &ViewSpec) -> bool {
    !schema
        .dataset(&view.dataset)
        .is_some_and(|d| d.is_computed())
}

/// The refusal for a view over a computed dataset — one text for the
/// `:view` command and the restore-fallback notice.
fn computed_view_refusal(view: &ViewSpec) -> String {
    format!(
        "view '{}' is over computed dataset '{}', which a module answers for; the blotter cannot show it",
        view.name, view.dataset
    )
}

/// Format an RFC 3339 freshness timestamp as `HH:MM` on the trader's clock.
/// Echo malformed values intact so unexpected input cannot panic by slicing.
fn short_time(t: &str, clock: geode_core::clock::Clock) -> String {
    match chrono::DateTime::parse_from_rfc3339(t) {
        Ok(at) => clock.hm(at.to_utc()),
        Err(_) => t.to_string(),
    }
}

/// Pinned chip text on the trader's configured clock: `AS OF HH:MM` for
/// today, or `AS OF YYYY-MM-DD HH:MM` for another date.
pub(crate) fn pinned_chip_text(
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    clock: geode_core::clock::Clock,
) -> String {
    let local = clock.local(at);
    if local.date_naive() == clock.local(now).date_naive() {
        format!("AS OF {}", local.format("%H:%M"))
    } else {
        format!("AS OF {}", local.format("%Y-%m-%d %H:%M"))
    }
}

/// Tooltip summary of the tile's filter: dimensions, text, then expression,
/// joined by ` · `; empty for an empty scope. Cached on construction and in
/// `set_tile_scope` so render only clones the prepared text.
pub(crate) fn filter_summary(scope: &Scope) -> String {
    let mut parts: Vec<String> = scope
        .dimensions
        .iter()
        .map(|d| format!("{} \u{2208} {}", d.column, d.values.join(", ")))
        .collect();
    if let Some(t) = &scope.text {
        parts.push(format!("text \"{t}\""));
    }
    if let Some(e) = &scope.expression {
        parts.push(e.to_string());
    }
    parts.join(" \u{b7} ")
}

impl gpui::Render for BlotterTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Record the first render after a snapshot is applied.
        if let Some(at) = self.delivered_at.take() {
            let micros = at.elapsed().as_micros() as u64;
            self.frame
                .update(cx, |f, _| f.requery.record_snapshot_to_paint(micros));
        }
        // Independently hosted tiles may have no `AppClock`; use the machine
        // clock as a fallback.
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        // A pinned instant's cached text depends on both the local date and
        // clock. Rebuild after midnight or a zone change, even if the zone
        // change leaves the date unchanged.
        if matches!(self.tile_as_of, TileAsOf::Pinned(AsOf::At(_)))
            && (clock.today(chrono::Utc::now()) != self.asof_chip_date
                || clock != self.asof_chip_clock)
        {
            self.refresh_asof_chip(clock);
        }
        // The footer strip's colors, memoized per summary and theme the
        // way the table's own header resolves them (a compare per frame).
        self.table.update(cx, |t, cx| {
            t.delegate_mut().ensure_summary_paint(cx.theme());
        });
        let theme = cx.theme();
        let delegate = self.table.read(cx).delegate();
        let snapshot = delegate.snapshot.clone();
        // Resolve chip and warning text through the shell's semantic paint
        // helper so theme foreground and fill remain a readable pair.
        let warn_chip = chip::chip_paint(theme, Tone::Warning);
        // Grouping pins, filters, and tile-local as-of pins use neutral chips.
        // Ignoring shared scope and following a historical frame use warning chips.
        let neutral_chip = chip::chip_paint(theme, Tone::Neutral);
        let warn_text = chip::chip_paint(theme, Tone::WarningText).text;

        // Header strip: view · grouping · markers · freshness · AS OF · … · error
        let mut header = h_flex()
            .w_full()
            .h(scale::design(HEADER_HEIGHT))
            .items_center()
            .gap_3()
            .px_2()
            .text_sm()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .debug_selector(|| format!("blotter-header-{}", self.tile.0));
        // The shared stack marker appears first when the tile belongs to a
        // stack of at least two members.
        header = header
            .children(self.stack.as_ref().and_then(|s| s.marker(theme, self.tile)))
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
                        .text_color(neutral_chip.text)
                        .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                        .px_1()
                        .rounded(theme.radius_tokens().sm)
                        .child("pinned"),
                )
            }
        }
        if self.unscoped {
            header = header.child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("blotter-unscoped"),
                        self.tile.0,
                    ))
                    .text_color(warn_chip.text)
                    .when_some(warn_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .child("unscoped")
                    .tooltip(tips::tip_with(
                        self.unscoped_tip_selector.clone(),
                        SharedString::new_static("Ignores the shared scope"),
                        None,
                        Some(SharedString::new_static(":unscoped re-attaches it")),
                    )),
            );
        }
        if !self.tile_scope.is_empty() {
            header = header.child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("blotter-filtered"),
                        self.tile.0,
                    ))
                    .text_color(neutral_chip.text)
                    .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .debug_selector(|| format!("blotter-filtered-{}", self.tile.0))
                    .child("filtered")
                    .tooltip(tips::tip_with(
                        self.filter_tip_selector.clone(),
                        self.filter_tip.clone(),
                        None,
                        Some(SharedString::new_static(":filter clear removes it")),
                    )),
            );
        }
        // A pinned as-of is a neutral state chosen by the trader. Read it from
        // tile state so the chip updates immediately, before the query returns.
        if let TileAsOf::Pinned(_) = &self.tile_as_of {
            header = header.child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("blotter-asof"),
                        self.tile.0,
                    ))
                    .text_color(neutral_chip.text)
                    .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .debug_selector(|| format!("blotter-asof-{}", self.tile.0))
                    .child(self.asof_chip.clone())
                    .tooltip(tips::tip_with(
                        self.asof_tip_selector.clone(),
                        self.asof_tip.clone(),
                        None,
                        Some(SharedString::new_static(":asof clear follows the frame")),
                    )),
            );
        }
        if let Some(snapshot) = &snapshot {
            let p = snapshot.provenance();
            let mut datasets: Vec<_> = p.datasets.iter().collect();
            datasets.sort_by(|a, b| a.as_of.cmp(&b.as_of));
            let now = chrono::Utc::now();
            for f in datasets {
                let text = match &f.as_of {
                    Some(t) => format!("{} {}", f.dataset, short_time(t, clock)),
                    None => format!("{} —", f.dataset),
                };
                let stale = self.is_stale(f.as_of.as_deref(), now);
                header = header.child(div().when(stale, |el| el.text_color(warn_text)).child(text));
            }
            // Historical request provenance warns only while following the
            // frame; a pinned tile already shows its own neutral as-of chip.
            if matches!(self.tile_as_of, TileAsOf::Follow)
                && let Some(req) = &p.as_of_request
            {
                let text = chrono::DateTime::parse_from_rfc3339(req)
                    .map(|t| format!("AS OF {}", clock.local(t.to_utc()).format("%Y-%m-%d %H:%M")))
                    .unwrap_or_else(|_| format!("AS OF {}", req.get(..16).unwrap_or(req)));
                header = header.child(
                    div()
                        .text_color(warn_chip.text)
                        .when_some(warn_chip.fill, |el, fill| el.bg(fill))
                        .px_1()
                        .rounded(theme.radius_tokens().sm)
                        .debug_selector(|| format!("blotter-asof-frame-{}", self.tile.0))
                        .child(text),
                );
            }
        }
        if self
            .following
            .in_flight_since()
            .is_some_and(|t| t.elapsed() > IN_FLIGHT_AFTER)
        {
            header = header.child(div().child("…"));
        }
        if let Some(n) = &self.error {
            header = header.child(notice::render(n, theme));
        }

        // Footer: counts and legends.
        let mut footer = h_flex()
            .w_full()
            .h(scale::design(FOOTER_HEIGHT))
            .items_center()
            .gap_4()
            .px_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .child(div().child(format!("{} rows", delegate.shown.len())))
            .when(delegate.selection_extent.is_some(), |f| {
                f.child(aggregates::strip(
                    delegate.selection_extent.as_ref(),
                    &delegate.summary,
                    &delegate.summary_paint,
                    theme,
                ))
            });
        // The dagger legend also covers a selection summary that carries
        // one: a per-row cell can be plain while the group it is folded
        // into is not (e.g. a determined-non-additive column at depth 1).
        let show_dagger = delegate.any_determined || delegate.summary_non_additive;
        if show_dagger {
            footer = footer.child(div().child("† shown for this row, do not total"));
        }
        if delegate.summary_unsummable {
            footer = footer.child(div().child(UNSUMMABLE_LEGEND));
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
                    .text_color(warn_text)
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
fn short_time_formats_an_rfc3339_instant_on_the_clock_and_echoes_garbage() {
    use geode_core::clock::Clock;
    let utc = Clock::utc();
    assert_eq!(short_time("2026-08-30T14:32:00Z", utc), "14:32");
    let shifted = Clock::in_zone_named("Asia/Tokyo");
    assert_eq!(
        short_time("2026-08-30T14:32:00Z", shifted),
        "23:32",
        "Tokyo is UTC+9"
    );
    // Not a time: echoed, never a panic.
    assert_eq!(short_time("2026", utc), "2026");
    assert_eq!(short_time("", utc), "");
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::groupings::GroupingSlots;
    use geode_core::query::{QueryKey, QueryOutcome};
    use geode_core::scopes::SavedScopes;
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::frame::{FLIP_DEADLINE, Frame, FrameRef, Publish};
    use geode_shell::module::{FindEvent, TileContent};
    use geode_shell::tiling::TileId;
    use geode_shell::tiling::WorkspaceIx;
    use geode_shell::vimfind::FindStyle;
    use geode_tile::notice::Notice;
    use gpui::px;
    use gpui::{Modifiers, MouseButton};
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    fn views() -> Vec<ViewSpec> {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\", \"underlying_ref\"]\n[[tree.columns]]\nname = \"delta01\"\n[[tree.columns]]\nname = \"daily_trading_pnl\"\n[wide]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[wide.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        ViewSpec::from_doc(&doc).0
    }

    /// Views with an explicit default, used to check that fresh tiles honor
    /// the flag independently of alphabetical view order.
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
                     [d.columns.daily_trading_pnl]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n\
                     [pricer]\ncomputed = true\n\
                     [pricer.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
                     [pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n";
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

    #[test]
    fn filter_summary_lists_dimensions_text_and_expression_in_order() {
        use geode_core::scope::DimensionSelection;

        let mut scope = Scope::default();
        scope.dimensions.push(DimensionSelection {
            column: "book".into(),
            values: vec!["A".into(), "B".into()],
        });
        scope.text = Some("abc".into());
        scope.expression = Some(parse_expr("npv > 0").unwrap());
        assert_eq!(
            filter_summary(&scope),
            "book \u{2208} A, B \u{b7} text \"abc\" \u{b7} npv > 0"
        );
        assert_eq!(filter_summary(&Scope::default()), "");
    }

    /// The column shape `snapshot` builds — factored out so
    /// `snapshot_with_as_of_request` can reuse it with a different
    /// `Provenance` rather than duplicating the columns.
    fn snapshot_columns() -> Vec<(ColumnMeta, TestColumn)> {
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta {
            name: n.into(),
            attribution_by_depth: by_depth,
            scope_semantics: ScopeSemantics::Direct,
            // The fixture's measures are plain sum measures.
            summable: matches!(n, "delta01" | "daily_trading_pnl"),
            mixed_flag: None,
        };
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
        ]
    }

    /// Root; L1, L2; L1/SPX. Trading PnL is NonAttributable at depth 2.
    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot::for_tests(snapshot_columns(), 2))
    }

    /// A snapshot with historical request provenance. This drives the frame's
    /// historical warning chip even if the selected data came from live tables;
    /// `as_of_request` does not identify which storage table served the query.
    fn snapshot_with_as_of_request(req: &str) -> Arc<Snapshot> {
        Arc::new(Snapshot::for_tests_with_provenance(
            snapshot_columns(),
            2,
            geode_core::snapshot::Provenance {
                datasets: vec![],
                as_of_request: Some(req.into()),
            },
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
            summable: matches!(n, "delta01" | "daily_trading_pnl"),
            mixed_flag: None,
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

    /// A third distinct payload for staged-snapshot races: `snapshot` is the
    /// baseline, `snapshot2` is the superseded answer, and this is the current
    /// answer. Distinct labels show which payload actually reaches the table.
    fn snapshot3() -> Arc<Snapshot> {
        let meta = |n: &str, by_depth: Vec<Attribution>| ColumnMeta {
            name: n.into(),
            attribution_by_depth: by_depth,
            scope_semantics: ScopeSemantics::Direct,
            summable: matches!(n, "delta01" | "daily_trading_pnl"),
            mixed_flag: None,
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
        /// The tile's own handle. `DataHandle::shutdown` on it is how a
        /// test makes the next submit be REFUSED (the bridge's own
        /// `a_refused_distinct_request_errors_the_picker` uses the same
        /// trick): there is no service behind a `for_tests` handle, so
        /// this only drops the sender.
        data: DataHandle,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    /// Open with a session record passed to `BlotterTile::new`, matching the
    /// factory's restoration path.
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
                    // The tile sits in unpinned workspace 1, so the shared lane
                    // is its lane and tests address it as `f.shared()` /
                    // `f.shared_mut()`. A test that pins must reach the tile's
                    // lane through its `FrameRef` instead.
                    let frame = cx.new(|_| Frame::new(slots(), SavedScopes::new(), None));
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                                data.clone(),
                                Rc::new(RefCell::new(views())),
                                Rc::new(RefCell::new(Arc::new(NamedColours::default()))),
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
                data,
            },
            vcx,
        )
    }

    /// Open with caller-supplied views to exercise default-view selection.
    fn open_with_views(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        views: Vec<ViewSpec>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_with_views_and_colours(cx, restored, views, NamedColours::default())
    }

    /// Open with shared colour definitions as supplied by the factory, so
    /// tests can inspect the definitions handed to the delegate.
    fn open_with_views_and_colours(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        views: Vec<ViewSpec>,
        colours: NamedColours,
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
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                                data.clone(),
                                Rc::new(RefCell::new(views)),
                                Rc::new(RefCell::new(Arc::new(colours))),
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
                data,
            },
            vcx,
        )
    }

    /// A fresh tile honors the explicit default view. Choosing "wide", which
    /// sorts after "tree", distinguishes the flag from alphabetical fallback.
    #[gpui::test]
    fn a_fresh_tile_opens_on_the_explicit_default_view_not_the_alphabetical_first(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, vcx) = open_with_views(cx, None, views_with_explicit_default("wide"));
        let state = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert_eq!(state["view"].as_str(), Some("wide"));
    }

    /// Views parsed from one builtin `views` layer, for the fixtures that
    /// mix the blotter's views with ones over the computed `pricer`.
    fn views_from(text: &str) -> Vec<ViewSpec> {
        ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin("views", text).unwrap()],
        ))
        .0
    }

    /// One blotter view over `d` beside one over the computed `pricer`
    /// dataset, as `views.toml` holds them side by side in the real app.
    fn tree_and_vanilla() -> Vec<ViewSpec> {
        views_from(
            "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[tree.columns]]\nname = \"delta01\"\n\
             [vanilla]\ndataset = \"pricer\"\n[[vanilla.columns]]\nname = \"npv\"\n",
        )
    }

    /// A computed view that sorts before every blotter view, with no
    /// `default`: the shape the shipped configuration has, where the
    /// pricer's `barrier` sorts before the demo's `tree`.
    fn apricot_and_tree() -> Vec<ViewSpec> {
        views_from(
            "[apricot]\ndataset = \"pricer\"\n[[apricot.columns]]\nname = \"npv\"\n\
             [tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[tree.columns]]\nname = \"delta01\"\n",
        )
    }

    /// The first-configured fallback skips a computed view, or a fresh
    /// tile would open on a view the blotter itself refuses.
    #[gpui::test]
    fn a_fresh_tile_skips_a_computed_view_that_sorts_first(cx: &mut gpui::TestAppContext) {
        let (h, vcx) = open_with_views(cx, None, apricot_and_tree());
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.view_name.clone()), "tree");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.error_text()), None);
    }

    /// A record naming the computed view that sorts first falls back past
    /// it to the first blotter view, with the refusal as its notice.
    #[gpui::test]
    fn restoring_a_computed_view_that_sorts_first_falls_back_past_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = "view = \"apricot\"".parse().unwrap();
        let (h, vcx) = open_with_views(cx, Some(&restored), apricot_and_tree());
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.view_name.clone()), "tree");
        let notice = h
            .tile
            .read_with(&vcx, |t, _| t.error_text())
            .expect("the fallback carries a notice");
        assert!(notice.contains("computed dataset 'pricer'"), "{notice}");
    }

    /// Switching views while the fallback's first query is still out
    /// drops the pending refusal: it was about the record, and the new
    /// view's first snapshot answers the trader, not the record.
    #[gpui::test]
    fn a_view_switch_drops_the_pending_restore_refusal(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = "view = \"vanilla\"".parse().unwrap();
        let views = views_from(
            "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[tree.columns]]\nname = \"delta01\"\n\
             [vanilla]\ndataset = \"pricer\"\n[[vanilla.columns]]\nname = \"npv\"\n\
             [wide]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n[[wide.columns]]\nname = \"delta01\"\n",
        );
        let (h, mut vcx) = open_with_views(cx, Some(&restored), views);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _first = next_query(&h.requests);
        h.tile.update_in(&mut vcx, |t, window, cx| {
            t.command("view wide", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        deliver(&h, &mut vcx, p.tag, Ok(snapshot()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.error_text()),
            None,
            "the new view's first snapshot carries no refusal about the record"
        );
    }

    /// With every configured view computed there is no fallback: the tile
    /// reports the unconfigured view and nothing stays pending for a
    /// delivery that can never come.
    #[gpui::test]
    fn an_all_computed_configuration_leaves_no_pending_refusal(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = "view = \"vanilla\"".parse().unwrap();
        let views =
            views_from("[vanilla]\ndataset = \"pricer\"\n[[vanilla.columns]]\nname = \"npv\"\n");
        let (h, mut vcx) = open_with_views(cx, Some(&restored), views);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.error_text()),
            Some("view '' is not configured".into())
        );
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.restored_view_refusal.is_none())
        );
    }

    /// A view over a computed dataset is a module's to answer: the blotter
    /// neither offers it in `:view` completion nor opens it by command.
    #[gpui::test]
    fn view_completion_and_command_exclude_views_over_a_computed_dataset(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_with_views(cx, None, tree_and_vanilla());
        let words = h
            .tile
            .read_with(&vcx, |t, cx| t.completions("view ", 5, cx));
        assert_eq!(
            words,
            vec!["tree"],
            "a computed dataset's view is not offered"
        );
        let err = h
            .tile
            .update_in(&mut vcx, |t, window, cx| {
                t.command("view vanilla", window, cx)
            })
            .unwrap_err();
        assert_eq!(
            err,
            "view 'vanilla' is over computed dataset 'pricer', which a module answers for; the blotter cannot show it"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.view_name.clone()),
            "tree",
            "the current view is kept"
        );
    }

    /// A session record naming a view over a computed dataset restores
    /// the default view instead, and says why in the tile's notice.
    #[gpui::test]
    fn restoring_a_view_over_a_computed_dataset_falls_back_to_the_default(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = "view = \"vanilla\"".parse().unwrap();
        let (h, vcx) = open_with_views(cx, Some(&restored), tree_and_vanilla());
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.view_name.clone()), "tree");
        let notice = h
            .tile
            .read_with(&vcx, |t, _| t.error_text())
            .expect("the fallback carries a notice");
        assert!(notice.contains("computed dataset 'pricer'"), "{notice}");
    }

    /// The fallback view's own first snapshot lands within milliseconds of
    /// opening; a notice it cleared would never be read. It outlives that
    /// delivery and clears on the next, which follows something the
    /// trader did.
    #[gpui::test]
    fn the_restore_fallback_notice_outlives_the_fallback_views_first_delivery(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = "view = \"vanilla\"".parse().unwrap();
        let (h, mut vcx) = open_with_views(cx, Some(&restored), tree_and_vanilla());
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut vcx, p.tag, Ok(snapshot()));
        let notice = h
            .tile
            .read_with(&vcx, |t, _| t.error_text())
            .expect("the notice survives the fallback view's first snapshot");
        assert!(notice.contains("computed dataset 'pricer'"), "{notice}");
        h.tile.update_in(&mut vcx, |t, window, cx| {
            t.command("group lhu", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        deliver(&h, &mut vcx, p.tag, Ok(snapshot()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.error_text()),
            None,
            "a delivery after the trader acted clears it"
        );
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
                    let colours = Rc::new(RefCell::new(Arc::new(NamedColours::default())));
                    let schema = Rc::new(RefCell::new(schema()));
                    let dims = Rc::new(RefCell::new(DerivedDimensions::default()));
                    cx.new(|cx| {
                        let a = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                                data.clone(),
                                views.clone(),
                                colours.clone(),
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
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                                data.clone(),
                                views.clone(),
                                colours.clone(),
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

    /// Freshness text uses the installed `AppClock` and reflects a later zone
    /// change. The helper mirrors render's uncached formatting; assertions use
    /// known Tokyo/UTC times independently of `Clock` formatting.
    #[gpui::test]
    fn the_freshness_readout_reads_the_installed_app_clock_and_follows_a_later_change(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(geode_shell::clock::AppClock(
                geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
            ))
        });
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let tag = next_query(&h.requests).tag;
        let meta = ColumnMeta {
            name: "lhu".into(),
            attribution_by_depth: vec![Attribution::Additive],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let provenance = Provenance {
            datasets: vec![Freshness {
                dataset: "risk".into(),
                as_of: Some("2026-09-12T14:00:00Z".into()),
                generation: Some(1),
            }],
            as_of_request: None,
        };
        let snapshot = Arc::new(Snapshot::for_tests_with_provenance(
            vec![(meta, TestColumn::Dict(vec![Some("X".into())]))],
            1,
            provenance,
        ));
        deliver(&h, &mut cx, tag, Ok(snapshot));

        let before = h.tile.read_with(&cx, |t, cx| t.freshness_texts(cx));
        assert_eq!(
            before,
            vec!["risk 23:00".to_string()],
            "Tokyo is UTC+9 on the 14:00:00Z fixture: {before:?}"
        );

        cx.update(|_window, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc()))
        });
        cx.run_until_parked();
        let after = h.tile.read_with(&cx, |t, cx| t.freshness_texts(cx));
        assert_eq!(
            after,
            vec!["risk 14:00".to_string()],
            "the observer refreshed and repainted: {after:?}"
        );
    }

    /// Changing `AppClock` repaints the cached pinned chip in the new zone.
    /// An explicit date avoids depending on the machine's current date. Set
    /// the cache's date to UTC's current date before switching so a date
    /// mismatch cannot mask a missing clock-key check. The text changes only
    /// when the observer schedules render and render refreshes the cache.
    #[gpui::test]
    fn the_pinned_chip_reads_the_installed_app_clock_and_follows_a_later_change(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(geode_shell::clock::AppClock(
                geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
            ))
        });
        let (h, mut cx) = open(cx);
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof 2030-06-15 13:00", window, cx).unwrap()
        });

        let before = h.tile.read_with(&cx, |t, _| t.asof_chip.to_string());
        assert_eq!(
            before, "AS OF 2030-06-15 13:00",
            "pinned at 13:00 in Tokyo: {before}"
        );

        let utc_today = geode_core::clock::Clock::utc().today(chrono::Utc::now());
        h.tile.update(&mut cx, |t, _| {
            t.asof_chip_date = utc_today;
        });

        cx.update(|_window, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc()))
        });
        cx.run_until_parked();
        let after = h.tile.read_with(&cx, |t, _| t.asof_chip.to_string());
        assert_eq!(
            after, "AS OF 2030-06-15 04:00",
            "the clock alone triggered the rebuild — its date matched already: {after}"
        );
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
            f.shared_mut().set_active_slot(Some(2));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["underlying_ref".to_string(), "lhu".into()][..])
        );
        assert!(h.requests.try_recv().is_err());

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("group lhu", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["lhu".to_string()][..]),
            "pinned"
        );
        h.frame.update(&mut cx, |f, cx| {
            f.shared_mut().set_active_slot(Some(1));
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a pinned tile does not follow the slot"
        );
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("unpin", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert_eq!(
            p.grouping.as_deref(),
            Some(&["lhu".to_string()][..]),
            "rejoined slot 1"
        );
    }

    /// A refused request immediately arrives at the barrier and clears
    /// `acted`, allowing a later notification to retry. This fixture shuts
    /// down the handle to force refusal without a future outcome.
    #[gpui::test]
    fn a_refused_query_arrives_at_the_barrier_and_retries_on_the_next_change(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let first = next_query(&h.requests);
        deliver(&h, &mut vcx, first.tag, Ok(snapshot()));

        // Nothing can be queued from here on: the handle's sender is
        // dropped, so the next submit is refused.
        h.data.shutdown();
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
            f.shared_mut()
                .open_flip([QueryKey(7)], std::time::Instant::now());
            cx.notify();
        });
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "a refusal must answer the barrier: nothing is coming for it"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.error.is_some()),
            "and it must still say so"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.following.acted().is_none()),
            "`acted` is cleared, so the next frame change is a real retry"
        );
    }

    /// A refusal names its kind: a full queue is busy and a later change
    /// can land; a stopped service is not coming back.
    #[gpui::test]
    fn a_refused_query_says_busy_or_stopped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let first = next_query(&h.requests);
        deliver(&h, &mut vcx, first.tag, Ok(snapshot()));
        let change = |vcx: &mut gpui::VisualTestContext| {
            h.frame.update(vcx, |f, cx| {
                f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
                cx.notify();
            });
        };
        let error = |vcx: &gpui::VisualTestContext| {
            h.tile
                .read_with(vcx, |t, _| t.error.as_ref().map(|e| e.text().to_string()))
        };
        h.data.fill_for_tests();
        change(&mut vcx);
        assert_eq!(
            error(&vcx).as_deref(),
            Some("query refused: the data service is busy")
        );
        h.data.shutdown();
        change(&mut vcx);
        assert_eq!(
            error(&vcx).as_deref(),
            Some("query refused: the data service has stopped")
        );
    }

    fn named(text: &str) -> geode_core::named::NamedExpressions {
        let doc = LayerDoc::builtin(geode_core::config::EXPRESSIONS_DOC, text).unwrap();
        let merged = merge_docs(geode_core::config::EXPRESSIONS_DOC, &[doc]);
        geode_core::named::NamedExpressions::from_doc(
            &merged,
            &geode_core::scope::complete::ExprVocab::default(),
        )
        .0
    }

    /// A frame scope naming an undefined expression is the tile's error,
    /// never a query: the barrier is answered (nothing is coming for it),
    /// an outcome still in flight for the previous scope cannot paint over
    /// it, and defining the name requeries with the expression folded in.
    #[gpui::test]
    fn an_unresolved_named_expression_errors_without_querying_and_a_definition_requeries(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let first = next_query(&h.requests);
        deliver(&h, &mut vcx, first.tag, Ok(snapshot()));

        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("A".into()));
            cx.notify();
        });
        let in_flight = next_query(&h.requests);

        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_scope(Scope {
                named: vec!["gone".into()],
                ..Scope::default()
            });
            f.shared_mut()
                .open_flip([QueryKey(7)], std::time::Instant::now());
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "an unresolved scope is never submitted"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.error.clone()),
            Some(Notice::danger("named expression 'gone' is missing"))
        );
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "nothing is coming for the barrier, so the tile answers it"
        );
        deliver(&h, &mut vcx, in_flight.tag, Ok(snapshot2()));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.error.clone())
                .map(|e| e.text().to_string()),
            Some("named expression 'gone' is missing".to_string()),
            "the previous scope's outcome is stale"
        );

        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.replace_named_expressions(named("[gone]\nexpression = \"npv > 0\"\n")));
            cx.notify();
        });
        let retry = next_query(&h.requests);
        assert!(retry.scope.named.is_empty(), "{:?}", retry.scope);
        assert_eq!(
            retry
                .scope
                .expression
                .as_ref()
                .map(|e| e.to_string())
                .as_deref(),
            Some("npv > 0")
        );
        deliver(&h, &mut vcx, retry.tag, Ok(snapshot()));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.error.clone()), None);
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
            summable: false,
            mixed_flag: None,
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

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("view wide", window, cx).unwrap()
        });
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Err("binder error".into()));
        let (rows, error) = h.tile.read_with(&cx, |t, cx| {
            (t.table().read(cx).delegate().shown.clone(), t.error.clone())
        });
        assert_eq!(rows, vec![0, 1, 2], "the last good snapshot stays");
        assert_eq!(
            error,
            Some(Notice::danger("binder error")),
            "a delivered error still paints danger"
        );
    }

    fn act(h: &Harness, cx: &mut gpui::VisualTestContext, id: &str) -> bool {
        h.tile
            .update(cx, |t, cx| t.dispatch(&ActionId(id.into()), None, cx))
    }
    fn clip(cx: &mut gpui::VisualTestContext) -> Option<String> {
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
    }
    fn delivered(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        (h, cx)
    }

    /// Find the centre of a painted element by its `debug_selector` id so
    /// mouse tests exercise the rendered cell. `debug_bounds` requires a
    /// `'static str`; all callers supply literal selectors.
    fn centre(cx: &mut gpui::VisualTestContext, sel: &'static str) -> gpui::Point<gpui::Pixels> {
        cx.run_until_parked();
        cx.debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} not painted"))
            .center()
    }

    /// [`centre`]'s element bounds, for a point beside the element.
    fn centre_bounds(
        cx: &mut gpui::VisualTestContext,
        sel: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        cx.run_until_parked();
        cx.debug_bounds(sel)
            .unwrap_or_else(|| panic!("{sel} not painted"))
    }

    #[gpui::test]
    fn shift_click_extends_a_block_from_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "motion::right"); // cursor (0, 1)
        let at = centre(&mut cx, "blotter-cell-2-2");
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::shift());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::shift());
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .unwrap();
        assert_eq!((r.kind, r.rows, r.cols), (SelectKind::Block, 0..3, 1..3));
        // The keyboard keeps extending what the mouse started.
        act(&h, &mut cx, "motion::up");
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .unwrap();
        assert_eq!(r.rows, 0..2);
    }

    /// A press on the row's trailing filler clears a live selection and
    /// moves the cursor to that row, including when it is already the cursor
    /// row. The row's press listener must report this gesture because no
    /// cell listener handles the filler.
    #[gpui::test]
    fn a_plain_click_beside_the_cells_clears_the_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let last = h.tile.read_with(&cx, |t, cx| {
            t.table()
                .read(cx)
                .delegate()
                .plan
                .as_ref()
                .unwrap()
                .columns
                .len()
                - 1
        });
        let beside = |cx: &mut gpui::VisualTestContext, row: usize| {
            let sel: &'static str =
                Box::leak(format!("blotter-cell-{row}-{last}").into_boxed_str());
            let b = centre_bounds(cx, sel);
            gpui::point(b.right() + gpui::px(40.), b.center().y)
        };
        let state = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                let d = t.table().read(cx).delegate();
                (d.selection.is_some(), d.cursor.row)
            })
        };
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down"); // rows 0..=1, cursor on 1
        assert_eq!(state(&mut cx), (true, 1), "fixture");

        let at = beside(&mut cx, 1);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        assert_eq!(state(&mut cx), (false, 1), "same row: the click clears");

        act(&h, &mut cx, "blotter::visual_rows");
        let at = beside(&mut cx, 2);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        assert_eq!(
            state(&mut cx),
            (false, 2),
            "another row: the click clears and moves"
        );
    }

    /// The tile's own paint resolves the footer's per-column colors: a
    /// selection over a measure, once drawn, has one paint per group.
    #[gpui::test]
    fn painting_the_tile_resolves_the_footer_colors(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down");
        cx.run_until_parked();
        let (groups, paints) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.summary.len(), d.summary_paint.len())
        });
        assert!(groups > 0, "the selection covers a measure");
        assert_eq!(paints, groups, "render memoized one paint per group");
    }

    /// Every live selection leads the footer with its extent — with or
    /// without a measure to summarise — prepared when the selection
    /// changes (render only reads it) and gone when the selection clears.
    #[gpui::test]
    fn a_selection_prepares_its_extent(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::visual_block"); // the tree column only
        act(&h, &mut cx, "motion::down");
        let extent = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                let d = t.table().read(cx).delegate();
                (d.summary.is_empty(), d.selection_extent.clone())
            })
        };
        assert_eq!(extent(&mut cx), (true, Some("2 rows × 1 col".into())));
        act(&h, &mut cx, "motion::right"); // into delta01: now summarised
        assert_eq!(extent(&mut cx), (false, Some("2 rows × 2 cols".into())));
        act(&h, &mut cx, "blotter::escape");
        assert_eq!(extent(&mut cx), (true, None));
    }

    /// The keyboard's own `SelectRow` echo (`sync_cursor` →
    /// `set_selected_row`) must never read as a plain click: `V j j`
    /// keeps its selection through every echo.
    #[gpui::test]
    fn the_cursor_echo_never_clears_a_keyboard_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down");
        act(&h, &mut cx, "motion::down");
        cx.run_until_parked();
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .expect("the selection survives its echoes");
        assert_eq!(r.rows, 0..3);
    }

    #[gpui::test]
    fn a_drag_selects_a_block_and_a_plain_click_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let from = centre(&mut cx, "blotter-cell-0-1");
        let to = centre(&mut cx, "blotter-cell-1-2");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .unwrap();
        assert_eq!((r.rows, r.cols), (0..2, 1..3));
        let elsewhere = centre(&mut cx, "blotter-cell-2-1");
        cx.simulate_mouse_down(elsewhere, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(elsewhere, MouseButton::Left, Modifiers::none());
        let (sel, cursor) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), (d.cursor.row, d.cursor.col))
        });
        assert_eq!((sel, cursor), (false, (2, 1)));
    }

    fn right_press(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = centre(cx, selector);
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    }

    /// gpui-component's table builds its context menu (empty here: the
    /// shell's row menu stands in for it) on every right press, and that
    /// menu's dismiss subscription holds it in a cycle only the table's
    /// next right press breaks, so a test ending after a right press leaks
    /// it. This breaks it: one more right press, whose deferred rebuild
    /// never runs because the window closes in the same update.
    fn release_the_table_menu(cx: &mut gpui::VisualTestContext) {
        let at = centre(cx, "blotter-cell-0-0");
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    button: MouseButton::Right,
                    position: at,
                    modifiers: Modifiers::none(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.remove_window();
        });
        cx.run_until_parked();
    }

    fn press_context(
        h: &Harness,
        cx: &mut gpui::VisualTestContext,
    ) -> Option<geode_core::context::DimensionContext> {
        h.tile.update(cx, |t, cx| t.press_context(cx))
    }

    /// [`delivered`] on a one-level view (`lhu`) that also shows the
    /// dimension `underlying_ref`, single-valued on each `lhu` row: root;
    /// L1 (SPX); L2 (NDX). Plan columns: tree, `underlying_ref`, `delta01`.
    fn delivered_with_a_dimension(
        cx: &mut gpui::TestAppContext,
    ) -> (Harness, gpui::VisualTestContext) {
        let text = "[flat]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[flat.columns]]\nname = \"underlying_ref\"\nkind = \"dimension\"\n\
                    [[flat.columns]]\nname = \"delta01\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let (h, mut cx) = open_with_views(cx, None, ViewSpec::from_doc(&doc).0);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        let meta = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: n == "delta01",
            mixed_flag: None,
        };
        let snapshot = Snapshot::for_tests(
            vec![
                (
                    meta("lhu"),
                    TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into())]),
                ),
                (
                    meta("underlying_ref"),
                    TestColumn::Dict(vec![None, Some("SPX".into()), Some("NDX".into())]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            1,
        );
        deliver(&h, &mut cx, p.tag, Ok(Arc::new(snapshot)));
        (h, cx)
    }

    /// A right press moves the cursor to the pressed cell and records the
    /// pressed column; `press_context` answers once, leading with that
    /// column when it is a dimension the row carries.
    #[gpui::test]
    fn a_right_press_moves_the_cursor_and_names_the_column(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        right_press(&mut cx, "blotter-cell-1-1");
        let (cursor, kind) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            let c = &d.plan.as_ref().unwrap().columns[1];
            assert_eq!(c.name, "underlying_ref");
            ((d.cursor.row, d.cursor.col), c.kind)
        });
        assert_eq!(cursor, (1, 1));
        assert_eq!(kind, ColumnKind::Dimension);
        let ctx = press_context(&h, &mut cx).expect("a pressed row has a context");
        assert_eq!(ctx.get("underlying_ref"), Some("SPX"));
        assert_eq!(ctx.first.as_deref(), Some("underlying_ref"));
        assert!(ctx.selection.is_empty());
        assert_eq!(press_context(&h, &mut cx), None, "the press is consumed");
        release_the_table_menu(&mut cx);
    }

    /// A measure names no first column: it is no dimension.
    #[gpui::test]
    fn a_right_press_on_a_measure_names_no_first_column(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        right_press(&mut cx, "blotter-cell-2-2");
        let ctx = press_context(&h, &mut cx).expect("a context");
        assert_eq!(ctx.get("underlying_ref"), Some("NDX"), "row 2's context");
        assert_eq!(ctx.first, None);
        release_the_table_menu(&mut cx);
    }

    /// The tree column names no first column, though its row carries `lhu`.
    #[gpui::test]
    fn a_right_press_on_the_tree_names_no_first_column(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        right_press(&mut cx, "blotter-cell-1-0");
        let ctx = press_context(&h, &mut cx).expect("a context");
        assert_eq!(ctx.get("lhu"), Some("L1"));
        assert_eq!(ctx.first, None);
        release_the_table_menu(&mut cx);
    }

    #[gpui::test]
    fn a_right_press_inside_the_selection_keeps_it(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| {
                d.cursor.row = 1;
                d.start_selection(SelectKind::Rows);
                d.cursor.row = 2;
            });
            t.sync_cursor(cx);
        });
        let before = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone());
        right_press(&mut cx, "blotter-cell-1-1");
        let (after, cursor_row) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.resolved.clone(), d.cursor.row)
        });
        assert_eq!(after, before, "the selection stays");
        assert_eq!(cursor_row, 2, "the cursor stays");
        let ctx = press_context(&h, &mut cx).unwrap();
        assert_eq!(ctx.selection.len(), 2, "the selected rows ride along");
        release_the_table_menu(&mut cx);
    }

    #[gpui::test]
    fn a_right_press_outside_the_selection_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| {
                d.cursor.row = 0;
                d.start_selection(SelectKind::Rows);
                d.cursor.row = 1;
            });
            t.sync_cursor(cx);
        });
        right_press(&mut cx, "blotter-cell-2-1");
        let (sel, cursor_row) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), d.cursor.row)
        });
        assert_eq!((sel, cursor_row), (false, 2));
        assert!(press_context(&h, &mut cx).unwrap().selection.is_empty());
        release_the_table_menu(&mut cx);
    }

    #[gpui::test]
    fn a_block_selection_fills_no_selection_context(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered_with_a_dimension(cx);
        h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| {
                d.cursor.row = 1;
                d.start_selection(SelectKind::Block);
                d.cursor.row = 2;
            });
            t.sync_cursor(cx);
        });
        let ctx = h
            .tile
            .read_with(&cx, |t, cx| {
                t.table().read(cx).delegate().dimension_context()
            })
            .unwrap();
        assert!(
            ctx.selection.is_empty(),
            "only a V (rows) selection rides along"
        );
    }

    #[gpui::test]
    fn the_cursor_row_records_its_anchor(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let anchor = |h: &Harness, cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            h.tile
                .read_with(cx, |t, cx| {
                    t.table().read(cx).delegate().dimension_context()
                })
                .unwrap()
                .anchor
                .expect("an anchor once painted")
        };
        let lower_left = |cx: &mut gpui::VisualTestContext, sel: &'static str| {
            let row = cx.debug_bounds(sel).expect("the row painted");
            (f32::from(row.left()), f32::from(row.bottom()))
        };
        // Within the row's 1px bottom border: the canvas fills the row
        // inside it.
        let near = |(ax, ay): (f32, f32), (rx, ry): (f32, f32)| {
            (ax - rx).abs() <= 1.0 && (ay - ry).abs() <= 1.0
        };
        let (at, row) = (anchor(&h, &mut cx), lower_left(&mut cx, "blotter-row-0"));
        assert!(near(at, row), "{at:?} vs {row:?}");
        act(&h, &mut cx, "motion::down");
        let (at, row) = (anchor(&h, &mut cx), lower_left(&mut cx, "blotter-row-1"));
        assert!(
            near(at, row),
            "the anchor follows the cursor row: {at:?} vs {row:?}"
        );
    }

    /// A drag whose press landed outside every cell and gutter (the
    /// header strip stands in for a scrollbar thumb, a header column
    /// reorder, a tile-split divider, or another tile's own text
    /// selection — anything that can hold the primary button down while
    /// the pointer later crosses this table) must never start or move a
    /// selection just because a later move happens to pass over a cell.
    #[gpui::test]
    fn a_drag_that_never_pressed_a_cell_selects_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let outside = centre(&mut cx, "blotter-header-7");
        let over = centre(&mut cx, "blotter-cell-1-2");
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(over, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(over, MouseButton::Left, Modifiers::none());
        let (sel, cursor) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), (d.cursor.row, d.cursor.col))
        });
        assert_eq!(
            (sel, cursor),
            (false, (0, 0)),
            "the press never landed on a cell or gutter, so a move over one starts nothing"
        );
    }

    /// A drag's selection kind is decided once, by where the press
    /// landed — a press on the gutter starts `Rows`, and dragging on from
    /// there across ordinary cells must not flip it to `Block`.
    #[gpui::test]
    fn a_gutter_drag_stays_rows_after_leaving_the_gutter(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        cx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::On,
            })
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let from = centre(&mut cx, "blotter-gutter-0");
        let to = centre(&mut cx, "blotter-cell-2-2");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .unwrap();
        assert_eq!((r.kind, r.rows), (SelectKind::Rows, 0..3));
    }

    /// A gutter press means `Rows`, not `Block` — the same distinction
    /// `V`/`v` draw with the keyboard — reached the same way a plain
    /// cell's does: through `on_ui_settings`'s production route
    /// (`UiSettings`, not a direct field write), which is what actually
    /// paints `blotter-gutter-2` for `centre` to find.
    #[gpui::test]
    fn a_gutter_shift_click_selects_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        cx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::On,
            })
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let at = centre(&mut cx, "blotter-gutter-2");
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::shift());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::shift());
        let r = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone())
            .unwrap();
        assert_eq!((r.kind, r.rows), (SelectKind::Rows, 0..3));
    }

    #[gpui::test]
    fn shift_v_selects_rows_and_y_copies_them_with_every_column(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down");
        act(&h, &mut cx, "blotter::yank");
        assert_eq!(
            clip(&mut cx).as_deref(),
            Some("lhu / underlying_ref\tdelta01\tdaily_trading_pnl\n\t9\t7\n  L1\t5\t7\n")
        );
        assert!(
            h.tile.read_with(&cx, |t, cx| t
                .table()
                .read(cx)
                .delegate()
                .selection
                .is_none()),
            "yank ends the selection"
        );
    }

    #[gpui::test]
    fn v_selects_a_block_and_y_copies_only_the_block(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "motion::right"); // cursor (0, delta01)
        act(&h, &mut cx, "blotter::visual_block");
        act(&h, &mut cx, "motion::down");
        act(&h, &mut cx, "motion::right"); // block rows 0..2 × cols 1..3
        act(&h, &mut cx, "blotter::yank");
        assert_eq!(
            clip(&mut cx).as_deref(),
            Some("delta01\tdaily_trading_pnl\n9\t7\n5\t7\n")
        );
    }

    #[gpui::test]
    fn the_other_key_switches_kind_and_the_same_key_clears(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        let kind = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                t.table()
                    .read(cx)
                    .delegate()
                    .selection
                    .as_ref()
                    .map(|s| s.kind)
            })
        };
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down");
        act(&h, &mut cx, "blotter::visual_block");
        assert_eq!(kind(&mut cx), Some(SelectKind::Block));
        let rows = h.tile.read_with(&cx, |t, cx| {
            t.table().read(cx).delegate().resolved.clone().unwrap().rows
        });
        assert_eq!(rows, 0..2, "the anchor survived the switch");
        act(&h, &mut cx, "blotter::visual_block");
        assert_eq!(kind(&mut cx), None);
    }

    #[gpui::test]
    fn escape_clears_the_selection_before_find(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        h.tile.update(&mut cx, |t, cx| {
            t.table()
                .update(cx, |t, _| t.delegate_mut().set_narrowed(Some(vec![1, 2])))
        });
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "blotter::escape");
        let (sel, narrowed) = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (d.selection.is_some(), d.narrowed.is_some())
        });
        assert_eq!(
            (sel, narrowed),
            (false, true),
            "first escape: selection only"
        );
        act(&h, &mut cx, "blotter::escape");
        assert!(h.tile.read_with(&cx, |t, cx| {
            t.table().read(cx).delegate().narrowed.is_none()
        }));
    }

    #[gpui::test]
    fn the_footer_sums_a_group_and_its_child_once(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "motion::down"); // L1
        act(&h, &mut cx, "blotter::expand"); // shown: root, L1, SPX, L2
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down"); // L1 + SPX
        let summary = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().summary.clone());
        let delta = summary
            .iter()
            .find(|c| c.label.as_ref() == "delta01")
            .expect("delta01 summarised");
        assert_eq!(delta.text.as_ref(), "5.00", "{delta:?}");
    }

    #[gpui::test]
    fn a_redelivery_keeps_the_selection_on_the_same_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "motion::down"); // L1
        act(&h, &mut cx, "blotter::visual_rows");
        act(&h, &mut cx, "motion::down"); // L1..L2 = rows 1..3
        // expand_all reflattens at once (SPX is materialised, so it lands
        // between L1 and L2) and always requeries.
        act(&h, &mut cx, "blotter::expand_all");
        let before = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone());
        assert_eq!(
            before.as_ref().map(|r| r.rows.clone()),
            Some(1..4),
            "anchor L1, cursor L2, by path"
        );
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let after = h
            .tile
            .read_with(&cx, |t, cx| t.table().read(cx).delegate().resolved.clone());
        assert_eq!(
            before, after,
            "a redelivery keeps the selection on the same rows"
        );
    }

    #[gpui::test]
    fn a_selection_whose_anchor_row_vanishes_clears_with_a_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = delivered(cx);
        act(&h, &mut cx, "motion::bottom"); // L2
        act(&h, &mut cx, "blotter::visual_rows");
        // Narrow to rows that exclude L2.
        h.tile.update(&mut cx, |t, cx| {
            t.table()
                .update(cx, |t, _| t.delegate_mut().set_narrowed(Some(vec![0, 1])));
            t.dispatch(&ActionId("motion::up".into()), None, cx);
        });
        let (sel, err) = h.tile.read_with(&cx, |t, cx| {
            (
                t.table().read(cx).delegate().selection.is_some(),
                t.error_text(),
            )
        });
        assert!(!sel);
        // The anchor here is the LAST row of the range (L2, with the
        // cursor moved up): the notice names the anchor, not "its first
        // row".
        assert_eq!(
            err.as_deref(),
            Some("selection cleared: anchor row no longer shown")
        );
    }

    /// A block whose anchor COLUMN is hidden by a view edit clears with a
    /// notice naming the column — its anchor row is still shown, so a
    /// row-worded notice would send the trader looking for the wrong
    /// thing.
    #[gpui::test]
    fn a_block_whose_anchor_column_is_hidden_clears_with_a_column_notice(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                     [[tree.columns]]\nname = \"model_code\"\nkind = \"dimension\"\n\
                     [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let views = ViewSpec::from_doc(&doc).0;
        let (h, mut cx) = open_with_views(cx, None, views);
        let meta = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (
                    meta("lhu"),
                    TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into())]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    meta("model_code"),
                    TestColumn::Dict(vec![None, Some("A".into()), Some("B".into())]),
                ),
                (
                    meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
            ],
            1,
        ));
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snap.clone()));

        // Anchor the block on model_code, then extend right onto delta01.
        let model_ix = h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| {
                d.plan.as_ref().unwrap().position_of("model_code").unwrap()
            })
        });
        h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| d.cursor.col = model_ix);
        });
        act(&h, &mut cx, "blotter::visual_block");
        act(&h, &mut cx, "motion::right");

        h.tile.update(&mut cx, |t, _| {
            t.views
                .borrow_mut()
                .iter_mut()
                .find(|v| v.name == "tree")
                .unwrap()
                .columns
                .retain(|c| c.name() != "model_code");
        });
        h.frame.update(&mut cx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Ok(snap));

        let (sel, err) = h.tile.read_with(&cx, |t, cx| {
            (
                t.table().read(cx).delegate().selection.is_some(),
                t.error_text(),
            )
        });
        assert!(!sel, "a block that lost its anchor column clears");
        assert_eq!(
            err.as_deref(),
            Some("selection cleared: anchor column no longer shown")
        );
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
        assert!(act(&mut cx, "motion::down", Some(2)));
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().cursor.row),
            2
        );
        act(&mut cx, "motion::up", None);
        // `ctrl+f`/`ctrl+b`: the ±10 step every dialog list has. Ten
        // outruns this snapshot, so it clamps to the last row, and
        // `ctrl+b` from there lands on row 0 — not on row -8.
        let row = |cx: &mut gpui::VisualTestContext| {
            h.tile
                .read_with(cx, |t, cx| t.table().read(cx).delegate().cursor.row)
        };
        assert!(act(&mut cx, "motion::page_down", None));
        assert_eq!(row(&mut cx), 2, "ctrl+f clamps to the last row");
        assert!(act(&mut cx, "motion::page_up", None));
        assert_eq!(row(&mut cx), 0, "ctrl+b clamps to the first row");
        act(&mut cx, "motion::down", Some(1));
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
        act(&mut cx, "motion::down", Some(2));
        act(&mut cx, "blotter::expand", None);
        let p = next_query(&h.requests);
        assert_eq!(
            p.max_depth, 2,
            "opening at the bound requeries one level deeper"
        );

        act(&mut cx, "motion::top", None);
        act(&mut cx, "blotter::visual_rows", None);
        act(&mut cx, "motion::down", Some(1));
        act(&mut cx, "blotter::yank", None);
        let clip = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
        assert_eq!(
            clip.as_deref(),
            Some("lhu / underlying_ref\tdelta01\tdaily_trading_pnl\n\t9\t7\n  L1\t5\t7\n")
        );
        assert!(
            h.tile.read_with(&cx, |t, cx| t
                .table()
                .read(cx)
                .delegate()
                .selection
                .is_none()),
            "yank leaves visual"
        );
        assert!(!act(&mut cx, "workspace::focus_left", None), "not ours");
    }

    /// A bare `j` past the last row wraps to row 0 in normal mode. In visual
    /// mode it clamps so extending a selection cannot wrap past its anchor.
    /// Neither movement changes the anchor.
    #[gpui::test]
    fn a_bare_j_wraps_in_normal_mode_and_clamps_in_visual(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let act = |cx: &mut gpui::VisualTestContext, id: &str| {
            h.tile
                .update(cx, |t, cx| t.dispatch(&ActionId(id.into()), None, cx))
        };
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 2]);

        assert!(act(&mut cx, "motion::bottom"));
        assert_eq!(cursor_row(&h, &cx), 2);
        assert!(act(&mut cx, "motion::down"));
        assert_eq!(cursor_row(&h, &cx), 0, "a bare j wraps in normal mode");

        assert!(act(&mut cx, "motion::bottom"));
        assert!(act(&mut cx, "blotter::visual_rows"));
        assert!(act(&mut cx, "motion::down"));
        assert_eq!(
            cursor_row(&h, &cx),
            2,
            "in visual mode the same keystroke clamps at the last row"
        );
        assert_eq!(
            h.tile.read_with(&cx, |t, cx| t
                .table()
                .read(cx)
                .delegate()
                .resolved
                .clone()
                .map(|r| r.rows)),
            Some(2..3),
            "and the selection anchor is intact"
        );
    }

    /// The blotter publishes `grid`, the flag the shell's shared motion
    /// bindings are written under; without it no motion key reaches it.
    #[gpui::test]
    fn the_key_context_publishes_the_grid_flag(cx: &mut gpui::TestAppContext) {
        let (h, cx) = open(cx);
        let ctx = h.tile.read_with(&cx, |t, cx| t.key_context(cx));
        assert!(ctx.has_flag(geode_shell::keymap::GRID));
        assert_eq!(ctx.get("mode"), Some("normal"));
    }

    /// Motions over a blotter with no rows yet change nothing.
    #[gpui::test]
    fn motions_on_an_empty_blotter_change_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        for (id, count) in [
            ("motion::down", None),
            ("motion::bottom", None),
            ("motion::bottom", Some(5)),
            ("motion::line_end", None),
            ("motion::up", Some(3)),
        ] {
            assert!(
                h.tile
                    .update(&mut cx, |t, cx| t.dispatch(&ActionId(id.into()), count, cx))
            );
            let (row, col) = h.tile.read_with(&cx, |t, cx| {
                let d = t.table().read(cx).delegate();
                (d.cursor.row, d.cursor.col)
            });
            assert_eq!((row, col), (0, 0), "{id} {count:?}");
        }
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
        // Each Fzf keystroke matches the full visible row list, rather than
        // the previous keystroke's narrowed positions.
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
    fn asof_and_sort_commands_and_completions(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof 14:05", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, geode_core::query::AsOf::At(_)));
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof clear", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live());

        let err = h
            .tile
            .update_in(&mut cx, |t, window, cx| {
                t.command("sort nonesuch", window, cx)
            })
            .unwrap_err();
        assert!(err.contains("nonesuch"));
        let words = h.tile.read_with(&cx, |t, cx| t.completions("sort ", 5, cx));
        assert_eq!(words, vec!["clear", "daily_trading_pnl", "delta01"]);
        let words = h.tile.read_with(&cx, |t, cx| t.completions("view ", 5, cx));
        assert_eq!(words, vec!["tree", "wide"]);
        let state = h.tile.read_with(&cx, |t, cx| t.serialize(cx));
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
                d.sort.as_ref().map(|s| (s.column.clone(), s.order))
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

        act(&mut cx, "motion::right");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), Some(("delta01".to_string(), SortOrder::Asc)));
        assert_eq!(header(&mut cx), "delta01");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::Desc))
        );
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), None);

        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsDesc))
        );
        assert_eq!(header(&mut cx), "delta01 |x|");
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsAsc))
        );
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(sort(&mut cx), None);
        assert_eq!(header(&mut cx), "delta01");

        // Crossing over: `s` from an absolute order restarts at asc, `S`
        // from a signed order restarts at abs desc.
        act(&mut cx, "blotter::sort_cycle_abs");
        act(&mut cx, "blotter::sort_cycle");
        assert_eq!(sort(&mut cx), Some(("delta01".to_string(), SortOrder::Asc)));
        act(&mut cx, "blotter::sort_cycle");
        act(&mut cx, "blotter::sort_cycle_abs");
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsDesc))
        );

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("sort daily_trading_pnl abs asc", window, cx)
                .unwrap()
        });
        assert_eq!(
            sort(&mut cx),
            Some(("daily_trading_pnl".to_string(), SortOrder::AbsAsc))
        );
        assert_eq!(
            header(&mut cx),
            "delta01",
            "the marker follows the sort column"
        );
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("sort delta01 abs", window, cx).unwrap()
        });
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsDesc))
        );
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("sort clear", window, cx).unwrap()
        });
        assert_eq!(sort(&mut cx), None);
    }

    /// Hiding the sorted column restores default row order and reports the
    /// dropped column. A configuration notification followed by redelivery
    /// exercises the same rebuild route as a Views-dialog edit.
    #[gpui::test]
    fn hiding_the_sorted_column_drops_the_sort_and_says_which(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("sort delta01", window, cx).unwrap()
        });
        assert!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().sort.is_some()),
            "precondition: the sort is set before the view edit"
        );

        // A view edit that hides delta01 shortens the plan.
        h.tile.update(&mut cx, |t, _| {
            t.views
                .borrow_mut()
                .iter_mut()
                .find(|v| v.name == "tree")
                .unwrap()
                .columns
                .retain(|c| c.name() != "delta01");
        });
        h.frame.update(&mut cx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Ok(snapshot()));

        assert!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).delegate().sort.is_none()),
            "the sorted column is gone, so the sort is gone"
        );
        let notice = h
            .tile
            .read_with(&cx, |t, _| t.error.clone())
            .expect("a notice names the dropped sort");
        assert!(
            notice.text().contains("delta01"),
            "the notice names the column whose sort went: {}",
            notice.text()
        );
        assert_eq!(
            notice.tone(),
            notice::Tone::Warning,
            "a dropped sort is a state change the trader caused, not an error"
        );
    }

    /// Hiding a column to the cursor's left must preserve its column identity.
    /// Its previous numeric index can remain in bounds while naming a different
    /// column; hiding a column to the right would not expose that mistake.
    #[gpui::test]
    fn hiding_a_column_left_of_the_cursor_carries_it_by_name(cx: &mut gpui::TestAppContext) {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                     [[tree.columns]]\nname = \"model_code\"\nkind = \"dimension\"\n\
                     [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n\
                     [[tree.columns]]\nname = \"daily_trading_pnl\"\nkind = \"measure\"\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let views = ViewSpec::from_doc(&doc).0;
        let (h, mut cx) = open_with_views(cx, None, views);

        let meta = |n: &str| ColumnMeta {
            name: n.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        };
        let snap = Arc::new(Snapshot::for_tests(
            vec![
                (
                    meta("lhu"),
                    TestColumn::Dict(vec![None, Some("L1".into()), Some("L2".into())]),
                ),
                (meta("row_depth"), TestColumn::I32(vec![0, 1, 1])),
                (
                    meta("model_code"),
                    TestColumn::Dict(vec![None, Some("A".into()), Some("B".into())]),
                ),
                (
                    meta("delta01"),
                    TestColumn::F64(vec![Some(9.0), Some(5.0), Some(4.0)]),
                ),
                (
                    meta("daily_trading_pnl"),
                    TestColumn::F64(vec![Some(7.0), Some(7.0), Some(7.0)]),
                ),
            ],
            1,
        ));

        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snap.clone()));

        // The plan is [tree, model_code, delta01, daily_trading_pnl];
        // rest the cursor on delta01, in the middle.
        let delta_ix = h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| {
                d.plan.as_ref().unwrap().position_of("delta01").unwrap()
            })
        });
        h.tile.update(&mut cx, |t, cx| {
            t.with_delegate(cx, |d| d.cursor.col = delta_ix);
        });

        // Hides model_code, to the LEFT of the cursor: delta01 and
        // daily_trading_pnl both shift down one slot.
        h.tile.update(&mut cx, |t, _| {
            t.views
                .borrow_mut()
                .iter_mut()
                .find(|v| v.name == "tree")
                .unwrap()
                .columns
                .retain(|c| c.name() != "model_code");
        });
        h.frame.update(&mut cx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        let p2 = next_query(&h.requests);
        deliver(&h, &mut cx, p2.tag, Ok(snap));

        let name = h.tile.read_with(&cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            d.plan.as_ref().unwrap().columns[d.cursor.col].name.clone()
        });
        assert_eq!(
            name, "delta01",
            "the cursor stays on the column it was on, not the position it held"
        );
    }

    /// Header clicks cycle every supported order, descending first, regardless
    /// of the component's proposed three-state order. Labels and the selected
    /// row follow the result; the tree column has no sort icon or sorting action.
    /// The test calls the delegate hook with deliberately wrong proposals.
    /// The component's private cached arrow still needs a real-window check.
    #[gpui::test]
    fn a_header_click_cycles_through_the_absolute_orders_too(cx: &mut gpui::TestAppContext) {
        use gpui_component::table::{ColumnSort, TableDelegate as _};
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        let click = |cx: &mut gpui::VisualTestContext, col: usize| {
            let table = h.tile.read_with(cx, |t, _| t.table().clone());
            cx.update(|window, cx| {
                table.update(cx, |t, cx| {
                    t.delegate_mut()
                        .perform_sort(col, ColumnSort::Ascending, window, cx)
                })
            });
            cx.run_until_parked();
        };
        let sort = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                t.table()
                    .read(cx)
                    .delegate()
                    .sort
                    .as_ref()
                    .map(|s| (s.column.clone(), s.order))
            })
        };
        let header = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                t.table().read(cx).delegate().column(1, cx).name.to_string()
            })
        };
        let act = |cx: &mut gpui::VisualTestContext, id: &str| {
            h.tile
                .update(cx, |t, cx| t.dispatch(&ActionId(id.into()), None, cx))
        };
        // (cursor row, the node id shown at it)
        let row_of = |cx: &mut gpui::VisualTestContext| {
            h.tile.read_with(cx, |t, cx| {
                let d = t.table().read(cx).delegate();
                (d.cursor.row, d.shown[d.cursor.row])
            })
        };

        // The tree column: no icon to click, and the hook refuses anyway.
        assert!(h.tile.read_with(&cx, |t, cx| {
            t.table().read(cx).delegate().column(0, cx).sort.is_none()
        }));
        click(&mut cx, 0);
        assert_eq!(sort(&mut cx), None, "the tree column cannot be sorted");

        // Rows: root 9, L1 5, L2 4, L1/SPX 5. Open L1 and put the cursor
        // on L2, so that an ascending sort moves L2 above L1's subtree.
        act(&mut cx, "motion::down");
        act(&mut cx, "blotter::expand");
        act(&mut cx, "motion::down");
        act(&mut cx, "motion::down");
        assert_eq!(
            row_of(&mut cx),
            (3, 2),
            "cursor on L2, below L1's open child"
        );

        click(&mut cx, 1);
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::Desc)),
            "first click: desc"
        );
        assert_eq!(row_of(&mut cx), (3, 2), "desc keeps L1 (5) above L2 (4)");
        click(&mut cx, 1);
        assert_eq!(sort(&mut cx), Some(("delta01".to_string(), SortOrder::Asc)));
        assert_eq!(header(&mut cx), "delta01");
        // Asc puts L2 (4) above L1 (5) and its open child: the cursor
        // follows L2 to row 1 by path, and so does the component's
        // highlight, through the deferred closure.
        assert_eq!(row_of(&mut cx), (1, 2), "cursor followed L2 up to row 1");
        assert_eq!(
            h.tile
                .read_with(&cx, |t, cx| t.table().read(cx).selected_row()),
            Some(1),
            "the component's highlight followed the cursor"
        );
        click(&mut cx, 1);
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsDesc))
        );
        assert_eq!(header(&mut cx), "delta01 |x|");
        click(&mut cx, 1);
        assert_eq!(
            sort(&mut cx),
            Some(("delta01".to_string(), SortOrder::AbsAsc))
        );
        click(&mut cx, 1);
        assert_eq!(sort(&mut cx), None);
        assert_eq!(header(&mut cx), "delta01");
        // Another column's click starts its own cycle at desc.
        click(&mut cx, 1);
        click(&mut cx, 2);
        assert_eq!(
            sort(&mut cx),
            Some(("daily_trading_pnl".to_string(), SortOrder::Desc))
        );
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

        h.a.update_in(&mut vcx, |t, window, cx| {
            t.command("filter model_code = 'EURP'", window, cx).unwrap()
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

        let state = h.a.read_with(&vcx, |t, cx| t.serialize(cx));
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
        h.a.update_in(&mut vcx, |t, window, cx| {
            t.command("filter clear", window, cx).unwrap()
        });
        let p2 = next_query(&h.requests);
        assert_eq!(p2.key, QueryKey(7));
        assert!(p2.scope.expression.is_none());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(vcx.debug_bounds("blotter-filtered-7").is_none());
    }

    /// Hovering the `filtered` pill shows the tile's own filter layer —
    /// the whole thing (`filter_summary`), not just the word "filtered".
    #[gpui::test]
    fn hovering_the_filtered_pill_shows_the_tiles_filter(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // A's initial query
        let _ = next_query(&h.requests); // B's initial query

        h.a.update_in(&mut vcx, |t, window, cx| {
            t.command("filter model_code = 'EURP'", window, cx).unwrap()
        });
        let _ = next_query(&h.requests);

        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let pill = vcx
            .debug_bounds("blotter-filtered-7")
            .expect("pill painted");
        vcx.simulate_mouse_move(
            pill.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("tip-blotter-filtered-7").is_some());
        let title = vcx
            .debug_bounds("tip-blotter-filtered-7-title")
            .expect("tooltip title painted");
        assert!(
            title.size.width > pill.size.width,
            "the filter text is longer than the word 'filtered'"
        );
    }

    /// `:filter text` updates the pill's cached hover summary, just as an
    /// expression filter does. It must describe the current tile scope.
    #[gpui::test]
    fn hovering_the_filtered_pill_after_filter_text_shows_the_text(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // A's initial query
        let _ = next_query(&h.requests); // B's initial query

        h.a.update_in(&mut vcx, |t, window, cx| {
            t.command("filter text underlying", window, cx).unwrap()
        });
        let _ = next_query(&h.requests);

        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let pill = vcx
            .debug_bounds("blotter-filtered-7")
            .expect("pill painted");
        vcx.simulate_mouse_move(
            pill.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("tip-blotter-filtered-7").is_some());
        let title = vcx
            .debug_bounds("tip-blotter-filtered-7-title")
            .expect("tooltip title painted");
        assert!(
            title.size.width > pill.size.width,
            "the text filter's own summary (`text \"underlying\"`) is longer than the word 'filtered'"
        );
    }

    /// `unscoped` drops the *frame's* layer, not the tile's own — a
    /// `:filter` still narrows the tile after `:unscoped`, and none of
    /// the frame's own dimension selections leak into the query's scope.
    #[gpui::test]
    fn an_unscoped_tile_still_applies_its_own_filter(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("unscoped", window, cx).unwrap()
        });
        let _ = next_query(&h.requests);

        // Give the frame a scope too, to prove it's excluded once
        // unscoped: an unscoped tile doesn't follow the frame's scope
        // version, so this alone triggers no requery.
        h.frame.update(&mut cx, |f, cx| {
            let mut scope = f.shared().scope().clone();
            scope.text = Some("ignored".into());
            if f.shared_mut().set_scope(scope) {
                cx.notify();
            }
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "unscoped: the frame's own scope change is not followed"
        );

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("filter text x", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert_eq!(p.scope.text.as_deref(), Some("x"));
        assert!(
            p.scope.dimensions.is_empty(),
            "no frame dimensions leaked in"
        );
    }

    /// `:asof <time>` pins only this tile, `:asof live` pins it to live even
    /// under a historical frame, and `:asof clear` resumes following. Pinning
    /// the frame's current value still changes ownership and requeries once;
    /// later frame changes do not requery the pinned tile.
    #[gpui::test]
    fn asof_pins_the_tile_and_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        // Pinning the frame's current value still stops following.
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof live", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(
            p.as_of.is_live(),
            "pinning to the frame's own live value still requeries once"
        );
        // The frame moves on to a new (historical) instant; a tile
        // pinned to the frame's own former value does not track it.
        h.frame.update(&mut cx, |f, cx| {
            f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a tile pinned to the frame's own value still does not follow it"
        );
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof clear", window, cx).unwrap()
        });
        let _ = next_query(&h.requests);

        let frame_as_of_version = h.frame.read_with(&cx, |f, _| f.shared().versions().as_of);

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof 14:05", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(
            matches!(p.as_of, AsOf::At(_)),
            "the request carries the pin"
        );
        assert!(
            !h.frame.read_with(&cx, |f, _| f.shared().as_of().is_live()),
            "the frame stayed at the historical instant the prelude moved it to"
        );
        assert_eq!(
            h.frame.read_with(&cx, |f, _| f.shared().versions().as_of),
            frame_as_of_version,
            "the frame's as-of counter did not move"
        );
        assert!(matches!(
            h.tile.read_with(&cx, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::At(_))
        ));

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof clear", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(
            matches!(p.as_of, AsOf::At(_)),
            "following again queries at the frame's (now historical) as-of"
        );

        // Pinning the same value again is a no-op: no requery.
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof clear", window, cx).unwrap()
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "clearing an already-following tile requeries nothing"
        );

        // Live under a historical frame.
        h.frame.update(&mut cx, |f, cx| {
            f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, AsOf::At(_)), "a following tile follows");
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof live", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live(), "pinned to live under a historical frame");
        assert!(
            matches!(
                h.frame.read_with(&cx, |f, _| f.shared().as_of().clone()),
                AsOf::At(_)
            ),
            "the frame stayed historical"
        );
    }

    /// An as-of pin ignores the frame's as-of changes without holding its
    /// barrier open: `on_frame_changed` records arrival without a new query.
    #[gpui::test]
    fn a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof 14:05", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.frame.update(&mut cx, |f, cx| {
            f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
            f.shared_mut()
                .open_flip([QueryKey(7)], std::time::Instant::now());
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a pinned tile does not requery on a frame as-of change"
        );
        assert!(
            !h.frame.read_with(&cx, |f, _| f.barrier_open()),
            "and it answered the barrier without a query"
        );
    }

    /// The pinned chip omits the date for today on the configured clock and
    /// includes it for other dates.
    #[test]
    fn pinned_chip_text_elides_todays_date() {
        let now = chrono::Utc::now();
        let clock = geode_core::clock::Clock::utc();
        let today = pinned_chip_text(now, now, clock);
        assert!(today.starts_with("AS OF "), "{today}");
        assert_eq!(today.len(), "AS OF HH:MM".len(), "{today}");
        let old = now - chrono::Duration::days(3);
        let past = pinned_chip_text(old, now, clock);
        assert_eq!(past.len(), "AS OF YYYY-MM-DD HH:MM".len(), "{past}");
    }

    /// The pinned chip paints from the tile's own state the moment the
    /// line runs, in place of the provenance-driven warning chip; a
    /// following tile under a historical frame paints only the latter.
    #[gpui::test]
    fn a_pinned_tile_paints_the_neutral_chip_and_hides_the_frame_one(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-asof-7").is_none(),
            "following, live: no chip"
        );

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof live", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        // A provenance carrying `as_of_request` — otherwise the frame
        // chip's `&& let Some(req) = &p.as_of_request` is already false
        // and the assertion below would pass whether or not the pinned
        // guard ahead of it does its job.
        deliver(
            &h,
            &mut cx,
            p.tag,
            Ok(snapshot_with_as_of_request("2026-01-01T00:00:00Z")),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-asof-7").is_some(),
            "pinned: the chip paints"
        );
        assert!(
            cx.debug_bounds("blotter-asof-frame-7").is_none(),
            "the provenance chip is suppressed while pinned"
        );

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof clear", window, cx).unwrap()
        });
        let _ = next_query(&h.requests);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-asof-7").is_none(),
            "cleared: no chip"
        );
    }

    /// Sessions store `as_of` only while pinned, as "live" or RFC 3339.
    /// Restoration preserves either pin and falls back to following when malformed.
    #[gpui::test]
    fn as_of_round_trips_through_the_session_record(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let state = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert!(state.get("as_of").is_none(), "following writes nothing");

        h.tile.update_in(&mut vcx, |t, window, cx| {
            t.command("asof live", window, cx).unwrap()
        });
        let state = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert_eq!(state["as_of"].as_str(), Some("live"));

        h.tile.update_in(&mut vcx, |t, window, cx| {
            t.command("asof 2026-09-20 14:05", window, cx).unwrap()
        });
        let state = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        let written = state["as_of"].as_str().unwrap().to_string();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&written).is_ok(),
            "{written}"
        );

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String(written.clone()));
        let (h2, mut vcx2) = open_with(cx, Some(&record));
        assert!(matches!(
            h2.tile.read_with(&vcx2, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::At(_))
        ));
        h2.tile.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h2.requests);
        assert!(
            matches!(p.as_of, AsOf::At(_)),
            "the first request carries the restored pin"
        );

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String("live".into()));
        let (h3, vcx3) = open_with(cx, Some(&record));
        assert_eq!(
            h3.tile.read_with(&vcx3, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::Live)
        );

        let mut record = toml::Table::new();
        record.insert("as_of".into(), toml::Value::String("yesterday-ish".into()));
        let (h4, vcx4) = open_with(cx, Some(&record));
        assert_eq!(
            h4.tile.read_with(&vcx4, |t, _| t.tile_as_of.clone()),
            TileAsOf::Follow,
            "a malformed value follows the frame"
        );
    }

    /// Render checks the pinned chip's date as well as edits to the pin.
    /// A stale cache date forces a refresh on the next draw, preventing a
    /// date-elided label from outliving the day it describes.
    #[gpui::test]
    fn the_pinned_chip_is_rebuilt_when_the_local_date_rolls_over(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("asof 14:05", window, cx).unwrap()
        });
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let at = h.tile.read_with(&cx, |t, _| match &t.tile_as_of {
            TileAsOf::Pinned(AsOf::At(at)) => *at,
            other => panic!("expected a pinned instant, got {other:?}"),
        });

        // Back-date the cache as if it were last built yesterday — the
        // guard must notice on the very next render.
        h.tile.update(&mut cx, |t, _| {
            t.asof_chip_date = t.asof_chip_date.pred_opt().unwrap();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // The fixture has no `AppClock`, so compare the rebuilt date and
        // text using the same machine-clock fallback as the tile.
        let clock = geode_core::clock::Clock::machine().0;
        let today = clock.today(chrono::Utc::now());
        h.tile.read_with(&cx, |t, _| {
            assert_eq!(t.asof_chip_date, today, "the guard rebuilt today's date");
            assert_eq!(
                t.asof_chip.to_string(),
                pinned_chip_text(at, chrono::Utc::now(), clock),
                "and the chip text with it"
            );
        });
    }

    /// Refused commands return the parser's message without changing tile state.
    #[gpui::test]
    fn refused_words_error_inline_with_the_doors_name(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        for (line, expected) in [
            ("scope lhu = 'L1'", crate::core::commands::REFUSED_SCOPE),
            ("asof undo", crate::core::commands::REFUSED_ASOF_UNDO),
            ("live", crate::core::commands::REFUSED_LIVE),
            ("group save 2", crate::core::commands::REFUSED_GROUP_SAVE),
        ] {
            let err = h
                .tile
                .update_in(&mut cx, |t, window, cx| t.command(line, window, cx))
                .unwrap_err();
            assert_eq!(err, expected, "`:{line}`");
        }
        assert!(
            h.requests.try_recv().is_err(),
            "a refusal requeries nothing"
        );
    }

    /// A bad `:filter` expression is reported inline at entry and leaves the
    /// active filter unchanged.
    #[gpui::test]
    fn filter_validates_against_the_tiles_dataset(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        let err = h
            .tile
            .update_in(&mut cx, |t, window, cx| {
                t.command("filter nope = 1", window, cx)
            })
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

    /// The stack marker appears first in the header only for stacks with
    /// multiple members. `title()` describes the same view and grouping shown
    /// in the header.
    #[gpui::test]
    fn the_stack_marker_paints_only_while_a_member(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut vcx, p.tag, Ok(snapshot()));
        assert!(vcx.debug_bounds("stack-marker-7").is_none());

        h.tile.update(&mut vcx, |t, cx| {
            t.set_stack(Some(StackHandle::new(2, 4, |_, _| {})), cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let marker = vcx.debug_bounds("stack-marker-7").expect("painted");
        let header = vcx.debug_bounds("blotter-header-7").unwrap();
        assert!(
            marker.left() - header.left() < px(20.0),
            "first in the strip"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.title()).as_ref(),
            "tree · lhu / underlying_ref"
        );

        // A one-member "stack" (defensive only — a live tree never keeps
        // one) is gated off exactly like no stack at all: `len > 1` is
        // the filter, not merely `is_some()`.
        h.tile.update(&mut vcx, |t, cx| {
            t.set_stack(Some(StackHandle::new(1, 1, |_, _| {})), cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("stack-marker-7").is_none(),
            "a stack of one paints no marker"
        );
    }

    /// Paints the tile and hands back the centre of one painted element
    /// by its debug selector — the mouse tests below click real bounds,
    /// never a synthesised event, so a listener that is not actually
    /// wired to the painted element fails them.
    fn centre_of(
        cx: &mut gpui::VisualTestContext,
        selector: &'static str,
    ) -> gpui::Point<gpui::Pixels> {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    /// A left mouse-down/up pair at `at` carrying `click_count` — gpui's
    /// own `simulate_click` hardwires a count of 1, and a double-click
    /// is nothing but the second press of a pair with a count of 2.
    fn click_at(
        cx: &mut gpui::VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        click_count: usize,
    ) {
        cx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
        });
    }

    fn shown_rows(h: &Harness, cx: &gpui::VisualTestContext) -> Vec<u32> {
        h.tile
            .read_with(cx, |t, cx| t.table().read(cx).delegate().shown.clone())
    }

    fn cursor_row(h: &Harness, cx: &gpui::VisualTestContext) -> usize {
        h.tile
            .read_with(cx, |t, cx| t.table().read(cx).delegate().cursor.row)
    }

    /// A double-click anywhere on a row is `space` on it: the cursor
    /// moves there and the node toggles. Row 1 is L1, whose child SPX is
    /// already materialised, so opening it needs no requery; the second
    /// double-click closes it again.
    #[gpui::test]
    fn a_double_click_on_a_row_toggles_it_like_space(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 2]);

        // A measure cell, well away from the chevron.
        let at = centre_of(&mut cx, "blotter-cell-1-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        assert_eq!(cursor_row(&h, &cx), 1, "the click moved the cursor");
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2], "L1 opened");
        assert!(
            h.requests.try_recv().is_err(),
            "no requery: SPX was in hand"
        );

        let at = centre_of(&mut cx, "blotter-cell-1-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 2], "L1 closed again");
    }

    /// Opening a node whose children the snapshot stopped short of
    /// requeries one level deeper — the same path `space` takes.
    #[gpui::test]
    fn a_double_click_at_the_depth_bound_requeries(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        // Row 2 is L2, which has no fetched child.
        let at = centre_of(&mut cx, "blotter-cell-2-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        let p = next_query(&h.requests);
        assert_eq!(
            p.max_depth, 2,
            "opening at the bound requeries one level deeper"
        );
    }

    /// A leaf has nothing to toggle: `space` on it is a no-op, and so is
    /// a double-click.
    #[gpui::test]
    fn a_double_click_on_a_leaf_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));
        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("motion::down".into()), None, cx);
            t.dispatch(&ActionId("blotter::expand".into()), None, cx);
        });
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2]);

        // Row 2 is now SPX, the leaf.
        let at = centre_of(&mut cx, "blotter-cell-2-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        assert_eq!(cursor_row(&h, &cx), 2);
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2], "nothing to toggle");
        assert!(h.requests.try_recv().is_err(), "a leaf never requeries");
    }

    /// A single click on the tree column's chevron toggles that row and
    /// moves the cursor to it, exactly as a double-click on the row does.
    #[gpui::test]
    fn a_chevron_click_toggles_the_row(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        let at = centre_of(&mut cx, "blotter-chevron-1");
        click_at(&mut cx, at, 1);
        assert_eq!(cursor_row(&h, &cx), 1, "the chevron click moved the cursor");
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2], "L1 opened");
        assert!(
            h.requests.try_recv().is_err(),
            "no requery: SPX was in hand"
        );

        let at = centre_of(&mut cx, "blotter-chevron-1");
        click_at(&mut cx, at, 1);
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 2], "L1 closed again");
    }

    /// A fast double-click that lands on the chevron toggles ONCE: the
    /// chevron's own listener acts on the first press only and stops the
    /// row's click from reaching the double-click path, so the pair can
    /// neither toggle twice (open, close) nor three times.
    #[gpui::test]
    fn a_double_click_on_the_chevron_toggles_once(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        let at = centre_of(&mut cx, "blotter-chevron-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        assert_eq!(
            shown_rows(&h, &cx),
            vec![0, 1, 3, 2],
            "L1 opened once and stayed open"
        );
    }

    /// A plain cell press clears a live selection, including the presses in
    /// a double-click. Toggling the row's expansion must not start a selection.
    #[gpui::test]
    fn a_double_click_that_toggles_a_row_leaves_no_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        let at = centre_of(&mut cx, "blotter-cell-1-1");
        click_at(&mut cx, at, 1);
        click_at(&mut cx, at, 2);
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2], "L1 opened once");
        assert!(
            !h.tile.read_with(&cx, |t, cx| t
                .table()
                .read(cx)
                .delegate()
                .selection
                .is_some()),
            "a double-click toggles the row; it never starts a selection"
        );
    }

    /// Likewise for the chevron's own press, underneath its `on_click`.
    #[gpui::test]
    fn a_chevron_click_leaves_no_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        let at = centre_of(&mut cx, "blotter-chevron-1");
        click_at(&mut cx, at, 1);
        assert_eq!(shown_rows(&h, &cx), vec![0, 1, 3, 2], "L1 opened");
        assert!(
            !h.tile.read_with(&cx, |t, cx| t
                .table()
                .read(cx)
                .delegate()
                .selection
                .is_some()),
            "the chevron toggles the row; it never starts a selection"
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
            summable: false,
            mixed_flag: None,
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

    /// A flat root-plus-leaves snapshot large enough for the virtualized
    /// viewport to show only a fraction of its rows.
    fn flat_snapshot(n: usize, delta_base: f64) -> Arc<Snapshot> {
        let meta = |name: &str| ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
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

    /// A `NonAttributable` cell has no value text. Without a pixel/text reader,
    /// the test checks the attribution in `FormatCache`, which selects the
    /// no-text render arm, and verifies that the cell element has painted bounds.
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
            t.dispatch(&ActionId("motion::down".into()), None, cx)
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

    /// A `DeterminedNonAdditive` cell supplies the cached attribution that
    /// renders the dagger and sets the footer's `any_determined` legend flag.
    #[gpui::test]
    fn a_determined_non_additive_cells_element_carries_the_dagger(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(attributed_snapshot()));

        h.tile.update(&mut cx, |t, cx| {
            t.dispatch(&ActionId("motion::down".into()), None, cx)
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

    /// The tree column stays fixed during horizontal scrolling. In a narrow
    /// window, `$` reveals the last column at the viewport's right edge and
    /// culls column 1 while the tree cell keeps its painted x coordinate.
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
            t.dispatch(&ActionId("motion::line_end".into()), None, cx)
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

    /// Publishing `[ui] line_numbers` through `UiSettings` updates a live tile
    /// without requerying. Off hides the gutter; relative mode shows cursor
    /// offsets with an absolute number at the cursor and updates after movement.
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
            t.dispatch(&ActionId("motion::down".into()), Some(2), cx)
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

    /// A new snapshot must refill formatted cells even when the virtualized
    /// visible row range stays unchanged. In that case `visible_rows_changed`
    /// does not fire again, so `invalidate_cells` must repopulate the cached
    /// range to avoid blank cells.
    #[gpui::test]
    fn a_cell_still_has_text_after_a_second_snapshot_with_an_unchanged_visible_range(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // the initial "tree" view query, unused
        h.tile.update_in(&mut cx, |t, window, cx| {
            t.command("view wide", window, cx).unwrap()
        });
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

    /// Two tiles stage outcomes until their shared frame barrier releases,
    /// then promote in the same notification pass. A failed outcome also
    /// arrives, allowing its sibling to promote while retaining its own last
    /// good snapshot and error. This fixture opens the barrier explicitly;
    /// shell tests cover opening it from frame changes.
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

        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());

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
            f.shared_mut().set_text(Some("A".into()));
            cx.notify();
        });
        let pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
            f.shared_mut().set_text(Some("B".into()));
            cx.notify();
        });
        let pa2 = next_query(&h.requests);
        let pb2 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
        assert_eq!(b_error, Some(Notice::danger("binder error")));
    }

    /// A grouping-pinned tile ignores grouping-only changes but still belongs
    /// to the barrier's participant set. `on_frame_changed` must arrive for it
    /// without submitting a query. Mutation and barrier creation precede the
    /// notification, matching the shell observer's ordering.
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
        h.a.update_in(&mut vcx, |t, window, cx| {
            t.command("group lhu", window, cx).unwrap()
        });
        let pa_pin = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa_pin.tag, Ok(snapshot()));

        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_active_slot(Some(1));
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now());
            cx.notify();
        });

        // Only B (unpinned) requeries; A ignores the grouping change.
        let pb1 = next_query(&h.requests);
        assert!(
            h.requests.try_recv().is_err(),
            "A is pinned — it never requeries"
        );

        let v = frame.read_with(&vcx, |f, _| f.shared().versions());
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

    /// Staged answers remain valid only for the tile's current query.
    /// A second scope change supersedes the staged answer; a grouping-only
    /// change leaves a grouping-pinned tile's stage valid; and a watched data
    /// publication invalidates it even while the original barrier remains open.
    ///
    /// `requery` clears stages and `promote` checks followed counters, including
    /// data and configuration. The separate tile-local requery test covers a
    /// change that the counter check alone cannot detect.
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

        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());

        // V1: a scope change opens a barrier over both keys.
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("V1".into()));
            cx.notify();
        });
        let pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        let _ = pa1; // A's V1 query is left outstanding — never delivered.
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
            f.shared_mut().set_text(Some("V2".into()));
            cx.notify();
        });
        let pa2 = next_query(&h.requests);
        let pb2 = next_query(&h.requests);
        let _ = pa2;
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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

        // B is pinned to a grouping, so a grouping-only second change neither
        // requeries nor invalidates its stage. The staged scope result remains
        // the answer to B's latest query and must promote on release.
        let (h2, mut vcx2) = open_two(cx);
        h2.a.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        h2.b.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        let qa0 = next_query(&h2.requests);
        let qb0 = next_query(&h2.requests);
        deliver_to(&h2.a, QueryKey(7), &mut vcx2, qa0.tag, Ok(snapshot()));
        deliver_to(&h2.b, QueryKey(8), &mut vcx2, qb0.tag, Ok(snapshot()));
        let baseline2 = shown_texts(&h2.b, &vcx2);

        h2.b.update_in(&mut vcx2, |t, window, cx| {
            t.command("group lhu", window, cx).unwrap()
        });
        let qb_pin = next_query(&h2.requests);
        deliver_to(&h2.b, QueryKey(8), &mut vcx2, qb_pin.tag, Ok(snapshot()));

        let frame2 = h2.a.read_with(&vcx2, |t, _| t.frame.entity().clone());

        // V1: a scope change — pinned-to-grouping B still follows scope,
        // so it requeries and, once the barrier opens over it, stages.
        frame2.update(&mut vcx2, |f, cx| {
            f.shared_mut().set_text(Some("V1".into()));
            cx.notify();
        });
        let qa1 = next_query(&h2.requests);
        let qb1 = next_query(&h2.requests);
        let _ = qa1; // A's V1 query is left outstanding — never delivered.
        frame2.update(&mut vcx2, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
            f.shared_mut().set_active_slot(Some(1));
            f.shared_mut().open_flip([QueryKey(8)], Instant::now());
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
            vec!["".to_string(), "M1".into(), "M2".into()],
            "B is pinned, so a grouping-only change must retain the V1 payload \
             answering its latest request"
        );

        // A watched data publication requeries B while its earlier answer is
        // staged. It leaves the original barrier open, but the earlier answer
        // must not promote on release: requery clears it, and promotion also
        // checks the followed data counter.
        let (h3, mut vcx3) = open_two(cx);
        h3.a.update(&mut vcx3, |t, cx| t.set_visible(true, cx));
        h3.b.update(&mut vcx3, |t, cx| t.set_visible(true, cx));
        let ra0 = next_query(&h3.requests);
        let rb0 = next_query(&h3.requests);
        deliver_to(&h3.a, QueryKey(7), &mut vcx3, ra0.tag, Ok(snapshot()));
        deliver_to(&h3.b, QueryKey(8), &mut vcx3, rb0.tag, Ok(snapshot()));
        let baseline3 = shown_texts(&h3.b, &vcx3);

        let frame3 = h3.a.read_with(&vcx3, |t, _| t.frame.entity().clone());

        // V1: a scope change opens a barrier over both keys.
        frame3.update(&mut vcx3, |f, cx| {
            f.shared_mut().set_text(Some("V1".into()));
            cx.notify();
        });
        let ra1 = next_query(&h3.requests);
        let rb1 = next_query(&h3.requests);
        let _ = ra1; // A's V1 query is left outstanding — the barrier
        // never releases on its own arrival in this scenario.
        frame3.update(&mut vcx3, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
                dataset: "d".into(),
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

    /// A hidden tile does not requery when followed counters change, so an
    /// existing stage can survive until release. Promotion must reject that
    /// stage based on its own version stamp.
    #[gpui::test]
    fn a_stage_is_dropped_when_a_counter_the_tile_follows_has_moved(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let baseline = shown_texts(&h.b, &vcx);
        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());

        // V1: a scope change over both keys; B stages, A never answers.
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("V1".into()));
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert_eq!(shown_texts(&h.b, &vcx), baseline, "V1 is staged");

        // B is hidden and the scope moves again: B follows `scope`, but a
        // hidden tile does not requery, so nothing clears the stage.
        h.b.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("V2".into()));
            cx.notify();
        });
        let _pa2 = next_query(&h.requests);
        assert!(
            h.requests.try_recv().is_err(),
            "a hidden tile asks for nothing"
        );
        frame.update(&mut vcx, |f, cx| {
            assert!(f.sweep(Instant::now() + FLIP_DEADLINE + Duration::from_millis(1)));
            cx.notify();
        });
        assert_eq!(
            shown_texts(&h.b, &vcx),
            baseline,
            "the stage answers a scope nobody is asking about any more"
        );
    }

    /// The blotter never cancelled on hide; this pins the shared rule: the
    /// reply lands while hidden and reshow asks nothing when nothing it
    /// follows moved.
    #[gpui::test]
    fn a_tile_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p0 = next_query(&h.requests);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        assert!(
            h.requests
                .try_iter()
                .all(|r| !matches!(r, Request::Cancel { .. })),
            "a hide is not a close"
        );
        deliver(&h, &mut vcx, p0.tag, Ok(snapshot()));
        assert_eq!(
            shown_texts(&h.tile, &vcx),
            vec!["".to_string(), "L1".into(), "L2".into()]
        );
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        assert!(h.requests.try_recv().is_err(), "nothing it follows moved");
    }

    #[gpui::test]
    fn a_followed_change_while_hidden_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p0 = next_query(&h.requests);
        deliver(&h, &mut vcx, p0.tag, Ok(snapshot()));
        h.tile.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("A".into()));
            cx.notify();
        });
        assert!(h.requests.try_recv().is_err(), "a hidden tile asks nothing");
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p1 = next_query(&h.requests);
        assert!(
            p1.tag > p0.tag,
            "the scope moved while hidden: reshow asks again"
        );
    }

    /// Closing a tile the barrier still waits on cancels its query and
    /// answers the barrier before its deadline, and the sibling that staged
    /// promotes in the same pass.
    #[gpui::test]
    fn closing_a_tile_mid_flip_cancels_its_query_and_releases_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        // The shell's recorder test pins that removal reaches `closed`;
        // this test pins what `closed` does.
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
        // The shell's order: the change and its barrier in one pass, then
        // each tile's observer.
        let opened = Instant::now();
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("A".into()));
            f.open_flip([QueryKey(7), QueryKey(8)], opened);
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        let old_texts = vec!["".to_string(), "L1".into(), "L2".into()];
        assert_eq!(
            shown_texts(&h.b, &vcx),
            old_texts,
            "B holds: A has not answered"
        );
        frame.update(&mut vcx, |f, _| {
            assert!(
                !f.sweep(opened + FLIP_DEADLINE / 2),
                "halfway to the deadline, time alone releases nothing"
            );
        });

        vcx.update(|_, cx| crate::content::BlotterContent::for_tile(h.a.clone()).closed(cx));
        assert!(
            h.requests
                .try_iter()
                .any(|r| matches!(r, Request::Cancel { key } if key == QueryKey(7))),
            "closing A cancels its query"
        );
        vcx.run_until_parked();
        assert!(
            !frame.entity().read_with(&vcx, |f, _| f.barrier_open()),
            "and answers the barrier before its deadline"
        );
        let new_texts = vec!["".to_string(), "M1".into(), "M2".into()];
        assert_eq!(
            shown_texts(&h.b, &vcx),
            new_texts,
            "B promotes in the pass the close released"
        );
    }

    /// A view a reload removed is this tile's error, and one broken tile
    /// never holds the rest open: the requery the reload triggers answers
    /// the open barrier at once instead of leaving it to the deadline. The
    /// question still out for the old view is superseded, so its late
    /// outcome cannot clear the error.
    #[gpui::test]
    fn a_view_removed_under_an_open_barrier_answers_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, _| t.view_name = "wide".into());
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
        let opened = Instant::now();
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("A".into()));
            f.open_flip([QueryKey(7), QueryKey(8)], opened);
            cx.notify();
        });
        let pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert!(
            frame.entity().read_with(&vcx, |f, _| f.barrier_open()),
            "B answered; A's question is still out"
        );
        frame.update(&mut vcx, |f, _| {
            assert!(
                !f.sweep(opened + FLIP_DEADLINE / 2),
                "halfway to the deadline, time alone releases nothing"
            );
        });
        assert!(frame.entity().read_with(&vcx, |f, _| f.barrier_open()));

        // The reload, as the app makes it: the factory's shared views are
        // replaced (`BlotterFactory::set_views`), then the config counter
        // moves, which is every tile's cue to requery.
        h.a.update(&mut vcx, |t, _| {
            t.views.borrow_mut().retain(|v| v.name != "wide")
        });
        frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.run_until_parked();
        let error = |vcx: &gpui::VisualTestContext| {
            h.a.read_with(vcx, |t, _| t.error.as_ref().map(|e| e.text().to_string()))
        };
        assert_eq!(
            error(&vcx).as_deref(),
            Some("view 'wide' is not configured")
        );
        assert!(
            !frame.entity().read_with(&vcx, |f, _| f.barrier_open()),
            "the unconfigured view answered the barrier before its deadline"
        );
        let answered_current = h.a.read_with(&vcx, |t, cx| {
            let now = t.versions(cx);
            t.following
                .acted()
                .is_some_and(|acted| !t.differs_on_followed(acted, now))
        });
        assert!(
            answered_current,
            "it answered as a new question under the reload's versions, so \
             an unrelated notification (the release's own) is not a retry"
        );

        deliver_to(&h.a, QueryKey(7), &mut vcx, pa1.tag, Ok(snapshot2()));
        assert_eq!(
            error(&vcx).as_deref(),
            Some("view 'wide' is not configured"),
            "a late outcome for the removed view paints nothing and clears nothing"
        );
    }

    /// A tile-local requery must clear the staged answer even when no frame
    /// counter changes. Commands such as `:filter`, `:group`, `:unpin`, and
    /// `:unscoped` change the question while leaving the counter gate satisfied;
    /// only the requery clear prevents the prior answer from painting.
    #[gpui::test]
    fn a_tile_local_requery_clears_the_stage_a_barrier_left_behind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let baseline = shown_texts(&h.b, &vcx);
        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());

        // V1: a scope change opens a barrier over both keys; A's query is
        // left outstanding, so only the deadline will release it.
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("V1".into()));
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert_eq!(
            shown_texts(&h.b, &vcx),
            baseline,
            "V1 is staged, not painted"
        );
        assert!(frame.read_with(&vcx, |f, _| f.barrier_open()));

        // B's own `:filter` — a tile-local question. It requeries, but no
        // frame counter moves, so the staged V1 payload and the frame
        // still agree on every counter B follows.
        h.b.update_in(&mut vcx, |t, window, cx| {
            t.command("filter text spx", window, cx).unwrap();
        });
        let pb_filter = next_query(&h.requests);
        assert_eq!(pb_filter.key, QueryKey(8));
        assert!(
            h.requests.try_recv().is_err(),
            "a tile-local filter asks for nothing on A's behalf"
        );
        let versions_agree = h.b.read_with(&vcx, |t, cx| {
            let now = t.frame.read(cx).versions();
            !t.differs_on_followed(t.following.acted().unwrap(), now)
        });
        assert!(
            versions_agree,
            "the premise: `promote`'s gate would wave this stage through — \
             the tile-local requery moved no counter B follows"
        );

        // The V1 barrier releases on its deadline and bumps `flip`.
        frame.update(&mut vcx, |f, cx| {
            assert!(f.sweep(Instant::now() + FLIP_DEADLINE + Duration::from_millis(1)));
            cx.notify();
        });
        assert_eq!(
            shown_texts(&h.b, &vcx),
            baseline,
            "the stage answers the pre-filter question — it must never \
             paint over a filtered query already in flight"
        );

        // The filtered outcome lands and paints, which is what B asked for.
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb_filter.tag, Ok(snapshot3()));
        assert_eq!(
            shown_texts(&h.b, &vcx),
            vec!["".to_string(), "N1".into(), "N2".into()],
            "the filtered payload paints once it actually arrives"
        );
    }

    /// Grouping and filter completions include dimensions absent from the
    /// current view. Here `model_code` is carried at instrument grain but is
    /// not displayed. Measures must never appear as grouping completions.
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
    }

    /// Applying a snapshot hands the factory's shared colour definitions to
    /// the delegate alongside the rebuilt plan. `cell_colour`, also used by
    /// cell and header rendering, must resolve against the current definitions.
    #[gpui::test]
    fn a_delivered_snapshot_hands_the_delegate_the_tiles_colours(cx: &mut gpui::TestAppContext) {
        let text = "[tree]\ndataset = \"d\"\ngrouping = [\"lhu\"]\n\
                    [[tree.columns]]\nname = \"delta01\"\nformat = { color = \"delta\" }\n";
        let doc = merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()]);
        let mut colours = NamedColours::default();
        colours.insert(
            "delta".into(),
            geode_core::colour::Definition::token(geode_core::colour::Token::Danger),
        );
        let (h, mut cx) =
            open_with_views_and_colours(cx, None, ViewSpec::from_doc(&doc).0, colours);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        let grey = geode_core::colour::Rgb {
            r: 0.5,
            g: 0.5,
            b: 0.5,
        };
        let danger = geode_core::colour::Rgb {
            r: 0.75,
            g: 0.125,
            b: 0.125,
        };
        let anchors = geode_core::colour::Anchors {
            normal: [grey; 6],
            light: [grey; 6],
        };
        let tokens = geode_core::colour::Tokens {
            foreground: grey,
            muted: grey,
            primary: grey,
            accent: grey,
            danger,
            warning: grey,
            success: grey,
            info: grey,
            chart: [grey; 5],
            bullish: grey,
            bearish: grey,
            background: grey,
        };
        let resolved = h.tile.update(&mut cx, |t, cx| {
            t.table().update(cx, |table, _| {
                table.delegate_mut().cell_colour(1, &anchors, &tokens)
            })
        });
        assert_eq!(
            resolved,
            Some(geode_tile::colour::Resolved::plain(
                geode_shell::shell::colours::to_hsla(danger)
            )),
            "the tile's own colours must reach the delegate with the plan"
        );
    }

    /// Every accepted or refused `:` command leaves the frame's scope,
    /// grouping, as-of, slots, and pending slot persistence unchanged. Reading
    /// `COMMANDS` makes a newly added command require an explicit test case.
    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        let lines = [
            "group lhu",
            "group slot 1",
            "unpin",
            // "unscoped" twice on purpose: toggle on, then toggle back
            // off, so every line after it still runs scoped.
            "unscoped",
            "unscoped",
            "filter lhu = 'L1'",
            "filter text spx",
            "filter clear",
            "asof 14:05",
            "asof live",
            "asof clear",
            "view wide",
            "sort delta01 desc",
            "sort clear",
            "autosize",
            "autosize reset",
            // The refusals.
            "scope lhu = 'L1'",
            "scope clear",
            "scope undo",
            "asof undo",
            "live",
            "group save 1",
        ];
        for word in crate::core::commands::COMMANDS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let read = |cx: &gpui::VisualTestContext| {
            h.frame.read_with(cx, |f, _| {
                (
                    f.shared().versions().scope,
                    f.shared().versions().grouping,
                    f.shared().versions().as_of,
                    f.slots().clone(),
                    f.shared().scope().clone(),
                    f.shared().as_of().clone(),
                )
            })
        };
        let before = read(&cx);
        for line in lines {
            let _ = h
                .tile
                .update_in(&mut cx, |t, window, cx| t.command(line, window, cx));
            while h.requests.try_recv().is_ok() {}
            assert_eq!(read(&cx), before, "`:{line}` reached the frame");
            assert!(
                h.frame
                    .update(&mut cx, |f, _| f.take_pending_persist())
                    .is_none(),
                "`:{line}` queued a slot write"
            );
            assert!(
                h.frame
                    .update(&mut cx, |f, _| f.take_pending_scope_persist())
                    .is_none(),
                "`:{line}` queued a scope write"
            );
        }
    }
    /// `snapshot()` with `delta01` carrying numbers far wider than the
    /// measure's configured width.
    fn wide_snapshot() -> Arc<Snapshot> {
        let mut columns = snapshot_columns();
        for (meta, col) in &mut columns {
            if meta.name == "delta01" {
                *col = TestColumn::F64(vec![Some(123_456_789_012_345.0); 4]);
            }
        }
        Arc::new(Snapshot::for_tests(columns, 2))
    }

    /// The width the delegate hands the table for the named column.
    fn column_width(h: &Harness, cx: &gpui::VisualTestContext, name: &str) -> f32 {
        use gpui_component::table::TableDelegate as _;
        h.tile.read_with(cx, |t, cx| {
            let d = t.table().read(cx).delegate();
            let ix = d.plan.as_ref().unwrap().position_of(name).unwrap();
            f32::from(d.column(ix, cx).width)
        })
    }

    /// `:autosize` through the command route widens a column whose cells
    /// outgrow its configured width; the fit survives the refresh a new
    /// delivery runs (the delegate prefers the fitted width over the
    /// plan's); `:autosize reset` returns to the configured width.
    #[gpui::test]
    fn autosize_fits_the_loaded_rows_survives_a_redelivery_and_resets(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        let configured = column_width(&h, &cx, "delta01");
        content_command(&h, &mut cx, "autosize").unwrap();
        let fitted = column_width(&h, &cx, "delta01");
        assert!(
            fitted > configured,
            "fitted {fitted} should exceed configured {configured}"
        );
        let rem = cx.update(|window, _| f32::from(window.rem_size()));
        let text_px = "123,456,789,012,345.00".chars().count() as f32 * rem * 0.875 * 0.6;
        assert!(fitted >= text_px, "{fitted} holds {text_px}px of text");

        // A redelivery rebuilds and refreshes: the fit stays.
        h.tile.update(&mut cx, |t, cx| t.requery(cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        assert_eq!(column_width(&h, &cx, "delta01"), fitted);

        content_command(&h, &mut cx, "autosize reset").unwrap();
        assert_eq!(column_width(&h, &cx, "delta01"), configured);
    }

    /// A `:` line through the shell's door, `TileContent::command`.
    fn content_command(
        h: &Harness,
        cx: &mut gpui::VisualTestContext,
        line: &str,
    ) -> Result<(), String> {
        use geode_shell::module::TileContent as _;
        let content = crate::content::BlotterContent::for_tile(h.tile.clone());
        cx.update(|window, cx| content.command(line, window, cx))
    }

    /// A record carrying fitted widths for the `tree` view.
    fn record_with_widths() -> toml::Table {
        let mut widths = toml::Table::new();
        widths.insert("delta01".into(), toml::Value::Float(200.0));
        widths.insert(String::new(), toml::Value::Float(150.0));
        let mut record = toml::Table::new();
        record.insert("view".into(), toml::Value::String("tree".into()));
        record.insert(
            geode_shell::colfit::SESSION_KEY.into(),
            toml::Value::Table(widths),
        );
        record
    }

    fn fitted_of(h: &Harness, cx: &gpui::VisualTestContext) -> geode_shell::colfit::FittedWidths {
        h.tile
            .read_with(cx, |t, cx| t.table().read(cx).delegate().fitted.clone())
    }

    /// With nothing loaded, `:autosize` refuses and leaves the restored
    /// widths alone; `:autosize reset` still drops them.
    #[gpui::test]
    fn autosize_with_nothing_loaded_refuses_and_keeps_the_widths(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open_with(cx, Some(&record_with_widths()));
        let before = fitted_of(&h, &cx);
        assert_eq!(before.len(), 2);
        assert_eq!(
            content_command(&h, &mut cx, "autosize"),
            Err(geode_shell::colfit::NOTHING_TO_FIT.to_string())
        );
        assert_eq!(fitted_of(&h, &cx), before);
        content_command(&h, &mut cx, "autosize reset").unwrap();
        assert!(fitted_of(&h, &cx).is_empty());
    }

    /// A record whose view no longer exists opens the fallback view without
    /// the old view's widths.
    #[gpui::test]
    fn a_restored_record_for_a_missing_view_drops_its_widths(cx: &mut gpui::TestAppContext) {
        let mut record = record_with_widths();
        record.insert("view".into(), toml::Value::String("gone".into()));
        let (h, cx) = open_with(cx, Some(&record));
        assert!(fitted_of(&h, &cx).is_empty());
    }

    /// A regroup reaches the delegate with the next snapshot: the tree
    /// column's fitted width goes (its labels and depths changed), the
    /// measures' stay.
    #[gpui::test]
    fn a_regroup_drops_only_the_tree_columns_fitted_width(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        content_command(&h, &mut cx, "autosize").unwrap();
        let fitted = fitted_of(&h, &cx);
        assert!(fitted.contains_key("") && fitted.contains_key("delta01"));

        content_command(&h, &mut cx, "group lhu").unwrap();
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        let after = fitted_of(&h, &cx);
        assert!(!after.contains_key(""), "{after:?}");
        assert_eq!(after.get("delta01"), fitted.get("delta01"));
    }

    /// A redelivery at the same grouping keeps the tree column's width.
    #[gpui::test]
    fn a_redelivery_at_the_same_grouping_keeps_the_tree_width(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        content_command(&h, &mut cx, "autosize").unwrap();
        let fitted = fitted_of(&h, &cx);
        h.tile.update(&mut cx, |t, cx| t.requery(cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(wide_snapshot()));
        assert_eq!(fitted_of(&h, &cx), fitted);
    }

    /// Fitted widths ride the session record and come back on restore; a
    /// view switch drops them; a garbled record restores none.
    #[gpui::test]
    fn autosize_widths_round_trip_the_session_and_a_view_switch_clears_them(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut vcx, p.tag, Ok(wide_snapshot()));
        h.tile
            .update_in(&mut vcx, |t, window, cx| t.command("autosize", window, cx))
            .unwrap();
        let fitted = column_width(&h, &vcx, "delta01");
        let state = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert!(
            state.contains_key(geode_shell::colfit::SESSION_KEY),
            "{state:?}"
        );

        let (h2, mut vcx2) = open_with(cx, Some(&state));
        h2.tile.update(&mut vcx2, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h2.requests);
        deliver(&h2, &mut vcx2, p.tag, Ok(snapshot()));
        assert_eq!(
            column_width(&h2, &vcx2, "delta01"),
            fitted,
            "restored before any refit, whatever the new rows hold"
        );

        h2.tile
            .update_in(&mut vcx2, |t, window, cx| {
                t.command("view wide", window, cx)
            })
            .unwrap();
        let state = h2.tile.read_with(&vcx2, |t, cx| t.serialize(cx));
        assert!(!state.contains_key(geode_shell::colfit::SESSION_KEY));

        let mut garbled = toml::Table::new();
        garbled.insert(
            geode_shell::colfit::SESSION_KEY.into(),
            toml::Value::String("wide".into()),
        );
        let (h3, vcx3) = open_with(cx, Some(&garbled));
        let state = h3.tile.read_with(&vcx3, |t, cx| t.serialize(cx));
        assert!(!state.contains_key(geode_shell::colfit::SESSION_KEY));
    }

    fn publish_for(frame: &Entity<Frame>, vcx: &mut gpui::VisualTestContext, dataset: &str) {
        frame.update(vcx, |f, cx| {
            f.note_published(geode_shell::frame::Publish {
                dataset: dataset.into(),
                batch: "EOD".into(),
                books: 1,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
    }

    #[gpui::test]
    fn publication_bursts_query_only_base_and_join_consumers(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());
        h.a.update(&mut vcx, |t, _| {
            let mut views = t.views.borrow_mut();
            views
                .iter_mut()
                .find(|v| v.name == "tree")
                .unwrap()
                .joins
                .push(geode_core::view::JoinSpec {
                    dataset: "joined".into(),
                    on: vec!["lhu".into()],
                    required: true,
                });
            views.iter_mut().find(|v| v.name == "wide").unwrap().dataset = "other".into();
        });
        h.b.update(&mut vcx, |t, _| t.view_name = "wide".into());
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        next_query(&h.requests);
        next_query(&h.requests);
        let mut counts = [0, 0];
        for _ in 0..32 {
            for (dataset, key) in [
                ("d", Some(7)),
                ("joined", Some(7)),
                ("other", Some(8)),
                ("unrelated", None),
            ] {
                publish_for(&frame, &mut vcx, dataset);
                if let Some(key) = key {
                    let request = next_query(&h.requests);
                    assert_eq!(request.key, QueryKey(key));
                    counts[(key - 7) as usize] += 1;
                }
                assert!(
                    h.requests.try_recv().is_err(),
                    "no unrelated tile requery for {dataset}"
                );
            }
        }
        assert_eq!(
            counts,
            [64, 32],
            "96 queries for 128 publications, rather than 256"
        );

        // A reload can change the dependency graph. Both the removed join and
        // the previous base must stop invalidating, and new dependencies start.
        h.a.update(&mut vcx, |t, _| {
            let mut views = t.views.borrow_mut();
            let view = views.iter_mut().find(|v| v.name == "tree").unwrap();
            view.dataset = "new_base".into();
            view.joins[0].dataset = "new_join".into();
        });
        frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        next_query(&h.requests);
        next_query(&h.requests);
        for dataset in ["d", "joined"] {
            publish_for(&frame, &mut vcx, dataset);
            assert!(h.requests.try_recv().is_err());
        }
        for dataset in ["new_base", "new_join"] {
            publish_for(&frame, &mut vcx, dataset);
            assert_eq!(next_query(&h.requests).key, QueryKey(7));
            assert!(h.requests.try_recv().is_err());
        }
        h.a.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        publish_for(&frame, &mut vcx, "new_base");
        for _ in 0..64 {
            publish_for(&frame, &mut vcx, "unrelated");
        }
        assert!(
            h.requests.try_recv().is_err(),
            "hidden consumers defer work"
        );
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        assert_eq!(
            next_query(&h.requests).key,
            QueryKey(7),
            "hidden consumer retained the relevant change"
        );
        assert!(h.requests.try_recv().is_err());
    }

    #[gpui::test]
    fn unrelated_publications_neither_answer_a_query_nor_discard_its_stage(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let a = next_query(&h.requests);
        let b = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, a.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, b.tag, Ok(snapshot()));
        let frame = h.a.read_with(&vcx, |t, _| t.frame.entity().clone());
        frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_text(Some("new scope".into()));
            f.shared_mut()
                .open_flip([QueryKey(7), QueryKey(8)], Instant::now());
            cx.notify();
        });
        let _a = next_query(&h.requests);
        let b = next_query(&h.requests);
        publish_for(&frame, &mut vcx, "unrelated");
        assert!(h.requests.try_recv().is_err());
        assert!(
            frame.read_with(&vcx, |f, _| f
                .barrier_wants(QueryKey(8), f.shared().versions())),
            "query still outstanding"
        );
        deliver_to(&h.b, QueryKey(8), &mut vcx, b.tag, Ok(snapshot2()));
        assert!(h.b.read_with(&vcx, |t, _| t.following.is_staged()));
        publish_for(&frame, &mut vcx, "unrelated");
        assert!(h.requests.try_recv().is_err());
        assert!(
            frame.read_with(&vcx, |f, _| f.barrier_open()),
            "A is still outstanding"
        );
        frame.update(&mut vcx, |f, cx| {
            f.arrived(QueryKey(7), f.shared().versions());
            cx.notify();
        });
        assert_eq!(
            shown_texts(&h.b, &vcx),
            vec!["", "M1", "M2"],
            "unrelated data must not discard a valid staged result"
        );
    }
}
