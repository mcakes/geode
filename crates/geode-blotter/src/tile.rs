//! One blotter tile (Phase 3 spec §6.5, §6.7, §6.8): observes the frame,
//! submits keyed queries through `DataHandle`, applies outcomes, records
//! timing, and paints the header strip, the table, and the footer.

use crate::core::commands::{AsOfArg, Command, Vocabulary, completions, parse, parse_as_of};
use crate::core::cursor::{Mode, selection};
use crate::core::find::FindState;
use crate::core::flatten::{SortOrder, SortSpec};
use crate::core::plan::ColumnKind;
use crate::core::yank::tsv;
use crate::delegate::{BlotterDelegate, ChevronClicked};
use geode_core::colour::NamedColours;
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
use geode_shell::frame::{Frame, FrameVersions, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::linenumbers::{LineNumbers, UiSettings};
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::chip::{self, Tone};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_shell::vimfind::{FindDirection, FindStyle};
use geode_shell::vimnav::NavCommand;
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

/// After this long without a result the header shows an in-flight glyph
/// (foundation §7.1's 50–200 ms affordance).
const IN_FLIGHT_AFTER: Duration = Duration::from_millis(50);
/// Header and footer strip heights, in pixels at the design rem
/// (`geode_shell::shell::scale`): the strips follow the font size with
/// the text they hold. The market-data panel's header shares the 22.
const HEADER_HEIGHT: f32 = 22.0;
const FOOTER_HEIGHT: f32 = 20.0;

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

/// The tile's as-of (command-line locality spec §3): the third override
/// beside [`Pin`] (grouping) and `tile_scope`/`unscoped` (scope). A
/// pinned tile queries at its own instant and does not follow the
/// frame's `as_of` counter; `:asof clear` returns it to `Follow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileAsOf {
    /// Query at the frame's as-of; requery when it changes.
    Follow,
    /// Query at this instant regardless of the frame.
    Pinned(AsOf),
}

pub struct BlotterTile {
    tile: TileId,
    frame: Entity<Frame>,
    data: DataHandle,
    views: Rc<RefCell<Vec<ViewSpec>>>,
    /// The named colours a `Colour::Named` column paints in (Part 2c
    /// §6.2) — shared with every other tile exactly as `views` is, and
    /// refreshed on `ConfigReloaded` by the same `BlotterFactory` door.
    /// Handed to the delegate in `apply`, the one place a plan is built
    /// or rebuilt, so the delegate's own `Arc` is never older than the
    /// snapshot it is painting.
    colours: Rc<RefCell<Arc<NamedColours>>>,
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
    /// The `unscoped` pill's own tooltip selector (`"tip-blotter-
    /// unscoped-{id}"`), built once here since it depends only on the
    /// tile id, never per render.
    unscoped_tip_selector: SharedString,
    /// The as-of override (spec §3.1); `Follow` on a fresh tile. Tests
    /// read it directly, the way they read `pin`.
    pub(crate) tile_as_of: TileAsOf,
    /// The pinned chip's text and tooltip title, cached when
    /// `tile_as_of` changes (`set_tile_as_of`) so `render` clones two
    /// `SharedString`s rather than formatting — the `filter_tip` rule.
    /// Both empty while `Follow`.
    asof_chip: SharedString,
    asof_tip: SharedString,
    /// The date, ON THE CLOCK BELOW, `asof_chip`/`asof_tip` were built
    /// for (review round 1, Important: the date-elided `AS OF HH:MM`
    /// form must not outlive its day). `render` compares this against
    /// today before painting and rebuilds through `refresh_asof_chip` on
    /// a mismatch, so a session pinned across midnight does not keep
    /// claiming "today" for an instant that no longer is. Meaningless
    /// while `Follow` (never read then — the chip strings are empty and
    /// nothing paints).
    pub(crate) asof_chip_date: chrono::NaiveDate,
    /// The clock `asof_chip_date` was computed on (final review,
    /// Important): `today` alone cannot tell a `[time] zone` reload from
    /// a no-op, since `Clock::today` returns the SAME date in most zone
    /// pairs — a reload that only changes the zone (never the date)
    /// would leave the wall-clock text stuck on the old zone until an
    /// unrelated midnight rollover or the next `:asof` edit. `render`'s
    /// guard checks this alongside the date, so either changing is a
    /// rebuild.
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
    /// The frame versions last acted on; `None` until the first query.
    acted: Option<FrameVersions>,
    publications: Vec<PublicationWatch>,
    tag: u64,
    last_grouping: Vec<String>,
    /// [`Self::title`]'s answer, cached so a stack-list row (which reads
    /// it every frame the list is open) never formats a `String`: kept
    /// in step with `view_name`/`last_grouping` at their one assignment
    /// site each (`requery`, right after `last_grouping` is set — a
    /// `view_name` change always runs through `requery` immediately
    /// after, so that one site covers both).
    title: SharedString,
    /// This tile's place in its stack (tile-stacks spec §5.1), painted in
    /// the header (Task 9); `None` while not a stack member.
    stack: Option<StackHandle>,
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
        colours: Rc<RefCell<Arc<NamedColours>>>,
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
        // `as_of` (command-line locality spec §3.5): `"live"`, an RFC 3339
        // instant, or absent (following). A value that is neither follows
        // the frame, logged like a restored `filter.expr` that no longer
        // parses.
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
        // `try_global`, not the bare `cx.global` (Task 5 ruling): a
        // module test fixture may never have installed `AppClock`.
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
            TableEvent::SelectRow(row) => {
                this.table.update(cx, |t, _| {
                    let d = t.delegate_mut();
                    d.cursor.to_row(*row, d.shown.len());
                });
                cx.notify();
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
        cx.observe(&frame, |this, _, cx| this.on_frame_changed(cx))
            .detach();
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();
        // `AppClock` (as-of dialog spec §6.1): the per-dataset freshness
        // readouts read the global fresh at paint time (see `render`) —
        // a bare notify is all THEY need. The pinned `AS OF` chip does
        // NOT (`asof_chip`/`asof_chip_date`/`asof_chip_clock`, review
        // round 1 and the final review): `render`'s own guard, not this
        // observer, is what notices the clock changed and rebuilds the
        // cache — this handler only has to get `render` to run again.
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
            acted: None,
            publications: Vec::new(),
            tag: 0,
            last_grouping: Vec::new(),
            title,
            stack: None,
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
    /// own scope expression is validated in the shell's
    /// `shell::scope_expr_view` (the `Set scope expression…` dialog; a
    /// tile's `:scope` word reached a frame-wide version of the same
    /// check until command-line locality closed that route 2026-09-20)
    /// — an unknown column or a bad operator on a derived dimension is a
    /// user error reported at the caret/column, not a silent no-op or a
    /// compiler error surfaced far downstream. `Ok(())` when the view or
    /// its dataset isn't resolvable
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

    /// The one place `tile_scope` is assigned (final review, spec §5.1):
    /// `filter_tip` is the tile's own filter, spelled out in full for a
    /// hover (`filter_summary`), and it must never drift from
    /// `tile_scope` itself — three separate assignment pairs (`:filter`,
    /// `:filter text`, `:filter clear`) each had their own chance to
    /// update one and forget the other. Building `filter_tip` from
    /// `scope` before moving it into `self.tile_scope` costs nothing
    /// extra: `filter_summary` already borrows its argument.
    fn set_tile_scope(&mut self, scope: Scope) {
        self.filter_tip = filter_summary(&scope).into();
        self.tile_scope = scope;
    }

    /// The chip text and tooltip title for `as_of` (spec §3.4).
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

    /// Rebuilds `asof_chip`/`asof_tip`/`asof_chip_date`/`asof_chip_clock`
    /// from `tile_as_of` — the one door: called whenever `tile_as_of`
    /// changes (`set_tile_as_of`) and lazily by `render` when the
    /// trader's date OR clock has moved since the cache was last built
    /// (review round 1, Important: the date-elided `AS OF HH:MM` form
    /// must not outlive its day; final review, Important: a `[time]`
    /// zone reload that lands on the SAME date as before — the common
    /// case — must not leave the wall-clock text painted in the old
    /// zone until an unrelated midnight or the next `:asof` edit, which
    /// is why the clock is part of the cache key too, not just the
    /// date).
    fn refresh_asof_chip(&mut self, clock: geode_core::clock::Clock) {
        let (chip, tip) = Self::asof_chip_strings(&self.tile_as_of, clock);
        self.asof_chip = chip;
        self.asof_tip = tip;
        self.asof_chip_date = clock.today(chrono::Utc::now());
        self.asof_chip_clock = clock;
    }

    /// Change the as-of override; `true` when it changed. The one door,
    /// so Task 3's chip cache cannot go stale.
    fn set_tile_as_of(&mut self, next: TileAsOf, clock: geode_core::clock::Clock) -> bool {
        if self.tile_as_of == next {
            return false;
        }
        self.tile_as_of = next;
        self.refresh_asof_chip(clock);
        true
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

    /// Which counters this tile follows (§4.1). Deliberately does not
    /// compare `now.flip`/`acted.flip` (Phase 4 §3.10): `flip` never means
    /// "requery" — it means "a staged snapshot this tile already has may
    /// now be promoted", which `on_frame_changed` checks on its own before
    /// ever reaching this method.
    fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        self.differs_on_followed(acted, now)
    }

    /// Whether `versions` and `now` disagree on any counter THIS tile
    /// follows — `scope` unless it is unscoped, `grouping` unless it is
    /// pinned, `as_of` unless the tile's own as-of is pinned (spec
    /// §3.3), and always watched `data`/`config`. The one comparison
    /// [`Self::follows_changed`] and [`Self::promote`]'s gate both go
    /// through (I-1, final whole-branch review), so "what this tile
    /// requeries for" and "what invalidates something it has already
    /// staged" can never drift apart.
    fn differs_on_followed(&self, versions: FrameVersions, now: FrameVersions) -> bool {
        (!self.unscoped && versions.scope != now.scope)
            || (self.pin == Pin::None && versions.grouping != now.grouping)
            || (matches!(self.tile_as_of, TileAsOf::Follow) && versions.as_of != now.as_of)
            || versions.data != now.data
            || versions.config != now.config
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
        let now = self.versions(cx);
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
            let awaiting = self.in_flight.is_some()
                && self
                    .acted
                    .is_some_and(|acted| acted.same_flip_identity(now));
            if !awaiting && self.frame.read(cx).barrier_wants(key, now) {
                self.frame.update(cx, |f, cx| {
                    if f.arrived(key, now) {
                        cx.notify();
                    }
                });
            }
        }
    }

    /// Apply a staged snapshot, if any (Phase 4 §3.10) — `deliver` when
    /// its own arrival didn't empty the barrier, or `on_frame_changed`
    /// once `flip` shows it did. A no-op when nothing is staged, so
    /// calling it on every `flip` bump costs nothing for a tile that
    /// never had to wait.
    ///
    /// Fix round 1, Finding 1: a staged snapshot is only ever valid while
    /// it still answers this tile's latest question. A second mutation
    /// within the same barrier window replaces the barrier before this
    /// tile's own fresh requery lands — when that happens, the `flip`
    /// bump that eventually releases the newer barrier must not promote a
    /// snapshot staged for a question that has since moved. Dropping it
    /// here keeps whatever is already on screen (last-good); the tile's
    /// own `requery` (already in flight by the time this runs —
    /// `follows_changed` fires in the same `on_frame_changed` pass) will
    /// paint the real answer when it lands.
    ///
    /// I-1 (final whole-branch review): the gate asks whether anything
    /// this tile FOLLOWS has moved, not whether the barrier's flip
    /// identity is unchanged. An `unscoped` tile under a scope change and
    /// a pinned tile under a grouping change never requery, so for them
    /// the replaced barrier came with no fresh answer at all and the
    /// identity check threw away the only one they would ever get —
    /// leaving the pre-mutation rows painted with `acted` claiming the
    /// tile was current. `requery` clears `staged`, so a staged snapshot is by construction the answer to
    /// the latest question asked.
    fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((snapshot, grouping, versions)) = self.staged.take() else {
            return;
        };
        let now = self.versions(cx);
        if !self.differs_on_followed(versions, now) {
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
            // The plan is (re)built from `view` here, so the definitions
            // its `Colour::Named` columns resolve against are refreshed
            // in the same breath — a reloaded `colours.toml` reaches the
            // paint on the requery every applied reload already triggers
            // (`Frame::note_config_reloaded`), never a frame behind it.
            let colours = Arc::clone(&self.colours.borrow());
            self.table.update(cx, |t, cx| {
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
        self.watch_view(&view, cx);
        let (grouping, scope, as_of, versions) = {
            let frame = self.frame.read(cx);
            let grouping = self.grouping(frame, &view);
            let scope = if self.unscoped {
                self.tile_scope.clone()
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
        self.title = Self::compute_title(&self.view_name, &self.last_grouping);
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
            // A refused submit means nothing is ever coming for these
            // versions (market-data Part 3 Task 6 review, MIN-3, fixed at
            // both sites under the mechanism rule): arrive, or an open
            // barrier (§3.10) holds every other following tile to
            // `FLIP_DEADLINE` waiting for an outcome that will never
            // exist. Then clear `acted`, so the next frame change is a
            // real retry rather than `follows_changed` deciding this tile
            // is already up to date — without it, one refusal (a full
            // queue during a burst) left the tile on last-good until
            // something else happened to move the frame.
            let key = QueryKey(self.tile.0);
            self.frame.update(cx, |f, cx| {
                if f.arrived(key, versions) {
                    cx.notify();
                }
            });
            self.acted = None;
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
            let now = self.versions(cx);
            if self.follows_changed(now) {
                self.requery(cx);
            }
        }
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
        let Some(name) = action.0.strip_prefix("blotter::") else {
            return false;
        };
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
                    // Spec §20.5: a bare j/k wraps in normal mode only.
                    let wrap = matches!(d.mode, Mode::Normal);
                    d.cursor.move_rows(len, cmd, count, wrap);
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
                // `try_global`, not the bare `cx.global` (Task 5 ruling,
                // `geode_shell::clock`'s own module doc): a module test
                // fixture may never have installed `AppClock`.
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
        // rank; `dimensions` is what `group`/`filter` complete from
        // (`core::commands::completions`).
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
        let views = self.views.borrow().iter().map(|v| v.name.clone()).collect();
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
        t
    }

    /// The session record's `as_of` value (spec §3.5): `None` while
    /// following, `"live"`, or the pinned instant in RFC 3339 (UTC).
    fn as_of_record(&self) -> Option<toml::Value> {
        match &self.tile_as_of {
            TileAsOf::Follow => None,
            TileAsOf::Pinned(AsOf::Live) => Some(toml::Value::String("live".into())),
            TileAsOf::Pinned(AsOf::At(at)) => Some(toml::Value::String(
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )),
        }
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

    /// The per-dataset freshness readout exactly as `render` builds it —
    /// the `"{dataset} {short_time}"` strings, in the same `f.as_of`
    /// order, read through the SAME `try_global` door `render` uses —
    /// so a test asserts on what a trader reads rather than reaching
    /// for the global itself (review finding, Task 7: no test installed
    /// `AppClock` before this, so a tile that ignored it would have
    /// passed everything else).
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

/// The `HH:MM` of an RFC 3339 `as_of` on the trader's clock, for the
/// header's per-dataset freshness readout; a string that is not an
/// instant is echoed whole rather than sliced (a `&t[11..16]` panicked
/// on short input once — a malformed freshness timestamp must never be
/// able to take the render thread down with it).
fn short_time(t: &str, clock: geode_core::clock::Clock) -> String {
    match chrono::DateTime::parse_from_rfc3339(t) {
        Ok(at) => clock.hm(at.to_utc()),
        Err(_) => t.to_string(),
    }
}

/// The pinned chip's text (spec §3.4): `AS OF HH:MM` when `at` falls on
/// today's date on the trader's clock, `AS OF YYYY-MM-DD HH:MM`
/// otherwise — the same rule the toolbar's readout uses. On the
/// CONFIGURED clock (`[time] zone`), not the machine's local zone,
/// because every displayed time is the trader's configured clock
/// (Phase 4a ruling, as-of dialog Part 2).
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

/// One line for the `filtered` pill's tooltip: the tile's own filter
/// layer (`:filter`, spec §3.7), spelled the way the scope bar spells
/// the frame's — dimensions, then text, then expression, joined by
/// ` · `; empty for an empty scope. Computed once wherever `tile_scope`
/// changes (`BlotterTile::new`, and the `:filter`/`:filter clear`
/// handlers), never per render — see `filter_tip` on `BlotterTile`.
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
        // The paint half of §7.1 (§6.8): the first render after a delivery.
        if let Some(at) = self.delivered_at.take() {
            let micros = at.elapsed().as_micros() as u64;
            self.frame
                .update(cx, |f, _| f.requery.record_snapshot_to_paint(micros));
        }
        // `try_global`, not the bare `cx.global` (Task 5 ruling): a
        // module test fixture may never have installed `AppClock`.
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        // Midnight rollover (review round 1, Important) AND a `[time]`
        // zone reload landing on the same date (final review, Important:
        // `Clock::today` returns the same `NaiveDate` for most zone
        // pairs, so the date alone cannot tell a reload from a no-op) —
        // either one leaves `asof_chip`/`asof_tip` stale: a pinned
        // today-instant that stays open past the trader's own midnight
        // must not keep painting the date-elided `AS OF HH:MM` form as
        // if it were still today, and a zone change must not keep
        // painting the OLD zone's wall clock until one of those two
        // things eventually happens to shake it loose. One date read and
        // one `Clock` comparison per render, a rebuild only when either
        // moved.
        if matches!(self.tile_as_of, TileAsOf::Pinned(AsOf::At(_)))
            && (clock.today(chrono::Utc::now()) != self.asof_chip_date
                || clock != self.asof_chip_clock)
        {
            self.refresh_asof_chip(clock);
        }
        let theme = cx.theme();
        let delegate = self.table.read(cx).delegate();
        let snapshot = delegate.snapshot.clone();
        // One door for every semantic chip and warning run in this header
        // (`geode_shell::shell::chip`): `warning_foreground` over a tint
        // of `warning` — the pairing this used to paint — is the
        // background family on a barely-tinted background, under 3:1 on
        // 30 of 44 bundled themes.
        let warn_chip = chip::chip_paint(theme, Tone::Warning);
        // `pinned` and `filtered` are the trader's own choices, not hazards:
        // neutral, so `unscoped` and `AS OF` — the two that really warn —
        // are the only warning-toned things in the strip.
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
        // The stack marker paints first, through the one builder every
        // module uses (`StackHandle::marker`, spec §5.1) — `None` while
        // the tile is not a member of a stack of two or more.
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
        // The as-of override's chip (spec §3.4): neutral, like `pinned`
        // and `filtered` — a state the trader chose. Painted from the
        // tile's own state, so it is right from the keystroke, not from
        // the next delivery.
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
            // The frame's historical warning (inherited danger) — only
            // while FOLLOWING; a pinned tile's request always carries its
            // pin and the neutral chip above already says so.
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
            .in_flight
            .is_some_and(|t| t.elapsed() > IN_FLIGHT_AFTER)
        {
            header = header.child(div().child("…"));
        }
        if let Some(e) = &self.error {
            header = header.child(
                div()
                    .text_color(chip::chip_paint(theme, Tone::DangerText).text)
                    .child(e.clone()),
            );
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
    use geode_shell::frame::{FLIP_DEADLINE, Frame, Publish};
    use geode_shell::module::FindEvent;
    use geode_shell::tiling::TileId;
    use geode_shell::vimfind::FindStyle;
    use gpui::px;
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

    /// Same shape as `snapshot`, but with a provenance carrying an
    /// `as_of_request` — the archive-read marker the frame's historical
    /// warning chip reads (`blotter-asof-frame-…`). `snapshot`'s default
    /// provenance never sets this, so a test asserting that chip stays
    /// hidden while pinned needs this fixture to be a real check rather
    /// than one where the guard's `&&` right-hand side is already false.
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

    /// Same as [`open_with`], but the views doc is the caller's own
    /// rather than the fixed [`views`] fixture — for
    /// [`views_with_explicit_default`] (Phase 4b Task 1 fix round 1,
    /// MIN-4).
    fn open_with_views(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        views: Vec<ViewSpec>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_with_views_and_colours(cx, restored, views, NamedColours::default())
    }

    /// [`open_with_views`] with the tile's shared `colours` cell filled
    /// too — the factory's own pairing (Part 2c §6.2), so a test can see
    /// what a tile actually hands its delegate.
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
                                frame.clone(),
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
                    let colours = Rc::new(RefCell::new(Arc::new(NamedColours::default())));
                    let schema = Rc::new(RefCell::new(schema()));
                    let dims = Rc::new(RefCell::new(DerivedDimensions::default()));
                    cx.new(|cx| {
                        let a = cx.new(|cx| {
                            BlotterTile::new(
                                TileId(7),
                                frame.clone(),
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
                                frame.clone(),
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

    /// Review finding (Task 7): no test installed `AppClock`, so a tile
    /// that hard-coded `Clock::machine()` — or whose `observe_global`
    /// handler were deleted — would have passed everything else. The
    /// freshness readout is built fresh every `render` (never cached),
    /// so `freshness_texts` mirrors that same path rather than exposing
    /// a stored field. `2026-09-12T14:00:00Z` is `23:00` in Tokyo
    /// (UTC+9) and `14:00` in UTC — both spelled by hand, not derived
    /// through `Clock` (the thing under test).
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
        };
        let provenance = Provenance {
            datasets: vec![Freshness {
                dataset: "risk".into(),
                as_of: Some("2026-09-12T14:00:00Z".into()),
                generation: 1,
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

    /// Final review, Important 1: the freshness readout above reads the
    /// global fresh on every call and could never tell "observer
    /// present" from "observer deleted" — but the PINNED `AS OF` chip
    /// IS cached (`asof_chip`/`asof_chip_date`/`asof_chip_clock`), so
    /// this is a real pin on both the `AppClock` observer and the
    /// cache's clock key, the shape the market-data and diagnostics
    /// tests already have. Pinned with an explicit DATE, not a bare
    /// `HH:MM` (`parse_as_of` would otherwise resolve "today" on
    /// whichever clock is installed at pin time — deterministic per
    /// run, but on a different date than the assertion strings below
    /// expect whenever the real machine's date differs from the one
    /// this comment was written against); `2030-06-15 13:00` Tokyo is
    /// `2030-06-15 04:00` UTC (Tokyo is UTC+9, same calendar day either
    /// way) — both hand-spelled, not derived through `Clock` (the thing
    /// under test). The cache's DATE is force-set to what UTC's
    /// `today(now)` will read a moment later, BEFORE the global switch
    /// (the same poke-the-field trick `the_pinned_chip_is_rebuilt_when_
    /// the_local_date_rolls_over` uses to simulate staleness): without
    /// it, `render`'s date half of the guard would ALSO fire whenever
    /// Tokyo's real "today" happens to run ahead of UTC's (true for up
    /// to nine hours of every real day) and mask whether the clock half
    /// — `asof_chip_clock`, this finding's whole fix — is doing
    /// anything at all. Deleting the observer, or dropping
    /// `asof_chip_clock` back out of `render`'s guard, fails this test
    /// (checked by hand, each once, reverted).
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
        h.tile.update(&mut cx, |t, cx| {
            t.command("asof 2030-06-15 13:00", cx).unwrap()
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

    /// Market-data Part 3 Task 6 review, MIN-3, fixed at both sites under
    /// the mechanism rule: a REFUSED submit (a full request queue, or a
    /// gone service) means no outcome will ever arrive for those versions.
    /// Left unanswered it holds an open flip barrier (§3.10) to
    /// `FLIP_DEADLINE`, and left with `acted` set it is never retried
    /// either — the tile sits on last-good until something else happens to
    /// move the frame.
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
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            f.open_flip([QueryKey(7)], std::time::Instant::now());
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
            h.tile.read_with(&vcx, |t, _| t.acted.is_none()),
            "`acted` is cleared, so the next frame change is a real retry"
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

    /// Spec §20.5: a bare `j` past the last row wraps to row 0 in normal
    /// mode only. In visual mode the same keystroke clamps, so a
    /// selection being extended downward cannot leap back to the top
    /// and silently invert itself — and the anchor is untouched either
    /// way.
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

        assert!(act(&mut cx, "blotter::bottom"));
        assert_eq!(cursor_row(&h, &cx), 2);
        assert!(act(&mut cx, "blotter::down"));
        assert_eq!(cursor_row(&h, &cx), 0, "a bare j wraps in normal mode");

        assert!(act(&mut cx, "blotter::bottom"));
        assert!(act(&mut cx, "blotter::visual"));
        assert!(act(&mut cx, "blotter::down"));
        assert_eq!(
            cursor_row(&h, &cx),
            2,
            "in visual mode the same keystroke clamps at the last row"
        );
        assert!(
            matches!(
                h.tile
                    .read_with(&cx, |t, cx| t.table().read(cx).delegate().mode),
                Mode::Visual { anchor: 2 }
            ),
            "and the selection anchor is intact"
        );
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
    fn asof_and_sort_commands_and_completions(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, geode_core::query::AsOf::At(_)));
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live());

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

    /// A header click reaches every order a measure can show, desc first
    /// (user ruling 2026-09-12), whatever three-state value gpui-component
    /// proposes; the header label follows; the tree column paints no sort
    /// icon and its hook is a no-op; and the component's row highlight
    /// follows the cursor's row across the resort. Drives the delegate
    /// hook the component's click handler calls, with a deliberately
    /// wrong proposal each time. Not observable here: the component's
    /// cached arrow after the deferred refresh (`col_groups` is private),
    /// so its direction on the 3rd/4th click is on the display-check list.
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
                    .map(|s| (s.column, s.order))
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
        act(&mut cx, "blotter::down");
        act(&mut cx, "blotter::expand");
        act(&mut cx, "blotter::down");
        act(&mut cx, "blotter::down");
        assert_eq!(
            row_of(&mut cx),
            (3, 2),
            "cursor on L2, below L1's open child"
        );

        click(&mut cx, 1);
        assert_eq!(
            sort(&mut cx),
            Some((1, SortOrder::Desc)),
            "first click: desc"
        );
        assert_eq!(row_of(&mut cx), (3, 2), "desc keeps L1 (5) above L2 (4)");
        click(&mut cx, 1);
        assert_eq!(sort(&mut cx), Some((1, SortOrder::Asc)));
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
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsDesc)));
        assert_eq!(header(&mut cx), "delta01 |x|");
        click(&mut cx, 1);
        assert_eq!(sort(&mut cx), Some((1, SortOrder::AbsAsc)));
        click(&mut cx, 1);
        assert_eq!(sort(&mut cx), None);
        assert_eq!(header(&mut cx), "delta01");
        // Another column's click starts its own cycle at desc.
        click(&mut cx, 1);
        click(&mut cx, 2);
        assert_eq!(sort(&mut cx), Some((2, SortOrder::Desc)));
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

    /// Hovering the `filtered` pill shows the tile's own filter layer —
    /// the whole thing (`filter_summary`), not just the word "filtered".
    #[gpui::test]
    fn hovering_the_filtered_pill_shows_the_tiles_filter(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // A's initial query
        let _ = next_query(&h.requests); // B's initial query

        h.a.update(&mut vcx, |t, cx| {
            t.command("filter model_code = 'EURP'", cx).unwrap()
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

    /// `Command::FilterText` recomputes `filter_tip` exactly as
    /// `Command::FilterExpr` does (`set_tile_scope`, final review, spec
    /// §5.1) — a `:filter text` line must show up in the pill's hover
    /// exactly as a `:filter <expr>` one does above, not the stale
    /// pre-filter tip a missed recompute would leave painted.
    #[gpui::test]
    fn hovering_the_filtered_pill_after_filter_text_shows_the_text(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests); // A's initial query
        let _ = next_query(&h.requests); // B's initial query

        h.a.update(&mut vcx, |t, cx| {
            t.command("filter text underlying", cx).unwrap()
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

    /// Command-line locality spec §3: `:asof <time>` pins THIS tile, the
    /// frame's own as-of untouched; `:asof live` pins it to live under a
    /// historical frame; `:asof clear` follows again. Spec §7's own
    /// case opens the test: pinning the frame's OWN current (live)
    /// value still stops following it — a real transition, so it
    /// requeries once, but the frame moving on afterward requeries
    /// nothing out of the now-pinned tile.
    #[gpui::test]
    fn asof_pins_the_tile_and_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);

        // Spec §7: pinning the frame's own value still stops following.
        // The frame is live and the tile follows; `:asof live` pins the
        // tile to that same (live) value — a real transition
        // (Follow → Pinned(Live)) — so it requeries once.
        h.tile
            .update(&mut cx, |t, cx| t.command("asof live", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(
            p.as_of.is_live(),
            "pinning to the frame's own live value still requeries once"
        );
        // The frame moves on to a new (historical) instant; a tile
        // pinned to the frame's own former value does not track it.
        h.frame.update(&mut cx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "a tile pinned to the frame's own value still does not follow it"
        );
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let _ = next_query(&h.requests);

        let frame_as_of_version = h.frame.read_with(&cx, |f, _| f.versions().as_of);

        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(
            matches!(p.as_of, AsOf::At(_)),
            "the request carries the pin"
        );
        assert!(
            !h.frame.read_with(&cx, |f, _| f.as_of().is_live()),
            "the frame stayed at the historical instant the prelude moved it to"
        );
        assert_eq!(
            h.frame.read_with(&cx, |f, _| f.versions().as_of),
            frame_as_of_version,
            "the frame's as-of counter did not move"
        );
        assert!(matches!(
            h.tile.read_with(&cx, |t, _| t.tile_as_of.clone()),
            TileAsOf::Pinned(AsOf::At(_))
        ));

        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(
            matches!(p.as_of, AsOf::At(_)),
            "following again queries at the frame's (now historical) as-of"
        );

        // Pinning the same value again is a no-op: no requery.
        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        assert!(
            h.requests.recv_timeout(Duration::from_millis(200)).is_err(),
            "clearing an already-following tile requeries nothing"
        );

        // Live under a historical frame.
        h.frame.update(&mut cx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        let p = next_query(&h.requests);
        assert!(matches!(p.as_of, AsOf::At(_)), "a following tile follows");
        h.tile
            .update(&mut cx, |t, cx| t.command("asof live", cx).unwrap());
        let p = next_query(&h.requests);
        assert!(p.as_of.is_live(), "pinned to live under a historical frame");
        assert!(
            matches!(
                h.frame.read_with(&cx, |f, _| f.as_of().clone()),
                AsOf::At(_)
            ),
            "the frame stayed historical"
        );
    }

    /// A pinned tile does not follow the frame's as-of (spec §3.3): a
    /// frame change neither requeries it nor holds the barrier for it —
    /// the tile self-arrives through `Frame::arrived`, as a pinned-
    /// grouping tile does.
    #[gpui::test]
    fn a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let _ = next_query(&h.requests);
        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.frame.update(&mut cx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            f.open_flip([QueryKey(7)], std::time::Instant::now());
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

    /// The pinned chip reads `AS OF HH:MM` for today on the trader's
    /// configured clock and carries the date otherwise (spec §3.4).
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

        h.tile
            .update(&mut cx, |t, cx| t.command("asof live", cx).unwrap());
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

        h.tile
            .update(&mut cx, |t, cx| t.command("asof clear", cx).unwrap());
        let _ = next_query(&h.requests);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("blotter-asof-7").is_none(),
            "cleared: no chip"
        );
    }

    /// Session (spec §3.5): `as_of` is written only while pinned, as
    /// `"live"` or RFC 3339, restored to the same pin, and a malformed
    /// value restores to `Follow`.
    #[gpui::test]
    fn as_of_round_trips_through_the_session_record(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert!(state.get("as_of").is_none(), "following writes nothing");

        h.tile
            .update(&mut vcx, |t, cx| t.command("asof live", cx).unwrap());
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(state["as_of"].as_str(), Some("live"));

        h.tile.update(&mut vcx, |t, cx| {
            t.command("asof 2026-09-20 14:05", cx).unwrap()
        });
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
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

    /// The pinned chip is a per-render date guard, not just a per-
    /// mutation cache (review round 1, Important): a stale
    /// `asof_chip_date` (as if the cache was last built yesterday) is
    /// corrected on the very next draw, so the elided `AS OF HH:MM` form
    /// cannot survive into a day it no longer describes.
    #[gpui::test]
    fn the_pinned_chip_is_rebuilt_when_the_local_date_rolls_over(cx: &mut gpui::TestAppContext) {
        let (h, mut cx) = open(cx);
        h.tile.update(&mut cx, |t, cx| t.set_visible(true, cx));
        let p = next_query(&h.requests);
        deliver(&h, &mut cx, p.tag, Ok(snapshot()));

        h.tile
            .update(&mut cx, |t, cx| t.command("asof 14:05", cx).unwrap());
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

        // No `AppClock` global is installed in this harness, so `render`
        // falls back to `Clock::machine()` (Task 5's `try_global`
        // fallback) — the same zone this comparison must read.
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

    /// The refusals (spec §5) reach the trader as the parser's message,
    /// and touch nothing.
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
                .update(&mut cx, |t, cx| t.command(line, cx))
                .unwrap_err();
            assert_eq!(err, expected, "`:{line}`");
        }
        assert!(
            h.requests.try_recv().is_err(),
            "a refusal requeries nothing"
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

    /// The stack marker (tile-stacks spec §5.1) paints only while the
    /// tile is a stack member with more than one member, first in the
    /// header strip, and `title()` reads from the same cache the header
    /// text itself paints.
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
            t.dispatch(&ActionId("blotter::down".into()), None, cx);
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
    /// the flip bumps, and `promote`'s gate would also reject the scope
    /// mismatch if it didn't. Part 2 pins B to a fixed grouping, so a
    /// *grouping-only* second mutation never makes B requery at all
    /// (`requery`'s clear never runs) — and, since I-1 (final
    /// whole-branch review) narrowed the gate from the barrier's flip
    /// identity to the counters the tile itself FOLLOWS, the V1 payload
    /// there is no longer stale at all: it is the only answer a pinned B
    /// will ever get for that scope, and it now promotes. What the gate
    /// still refuses is a stage whose own followed counters moved, which
    /// `a_stage_is_dropped_when_a_counter_the_tile_follows_has_moved`
    /// pins. Part 3 is the reverse: a `data`-only bump (which never
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
            vec!["".to_string(), "M1".into(), "M2".into()],
            "I-1 (final whole-branch review): B is PINNED, so a \
             grouping-only mutation is not something it follows — the V1 \
             payload is still the answer to its latest question, and the \
             only answer it will ever get for that scope. Dropping it left \
             the pre-V1 rows painted with `acted` claiming B was current"
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

    /// I-1 (final whole-branch review), the other half of the same gate:
    /// a stage whose own followed counters have MOVED must still be
    /// dropped. Reachable while hidden — `set_visible(false)` leaves a
    /// stage in place and a hidden tile never requeries, so nothing else
    /// supersedes it; `promote`'s gate is the only thing between it and
    /// the screen.
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
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());

        // V1: a scope change over both keys; B stages, A never answers.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("V1".into()));
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
        });
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        assert_eq!(shown_texts(&h.b, &vcx), baseline, "V1 is staged");

        // B is hidden and the scope moves again: B follows `scope`, but a
        // hidden tile does not requery, so nothing clears the stage.
        h.b.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("V2".into()));
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

    /// The half of Fix round 1, Finding 1 that `promote`'s gate cannot
    /// reach, and the one `requery`'s clear is now alone in defending.
    ///
    /// Every scenario in
    /// `a_second_mutation_during_a_barrier_wait_clears_the_stale_staged_snapshot`
    /// moves a counter the tile FOLLOWS, and since I-1 (final
    /// whole-branch review) narrowed `promote`'s gate onto
    /// `differs_on_followed` — `data` and `config` included — the gate
    /// now catches all three on its own, Part 3's data-only bump
    /// included: the clear is masked there. A TILE-LOCAL requery is the
    /// case it is not. `:filter`, `:group`, `:unpin` and `:unscoped` all
    /// requery against frame versions that never move, so a stage made
    /// before one still agrees with `now` on every followed counter and
    /// the gate waves it through — only the clear stops a snapshot
    /// answering the pre-filter question from painting over the filtered
    /// query already in flight.
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
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());

        // V1: a scope change opens a barrier over both keys; A's query is
        // left outstanding, so only the deadline will release it.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("V1".into()));
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        frame.update(&mut vcx, |f, _| {
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now())
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
        h.b.update(&mut vcx, |t, cx| {
            t.command("filter text spx", cx).unwrap();
        });
        let pb_filter = next_query(&h.requests);
        assert_eq!(pb_filter.key, QueryKey(8));
        assert!(
            h.requests.try_recv().is_err(),
            "a tile-local filter asks for nothing on A's behalf"
        );
        let versions_agree = h.b.read_with(&vcx, |t, cx| {
            let now = t.frame.read(cx).versions();
            !t.differs_on_followed(t.acted.unwrap(), now)
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

    /// Regression: `BlotterTile::completions` used to build its
    /// `Vocabulary` from the column plan/view alone (what's
    /// *displayed*), so `:group `/`:filter ` never offered a dimension
    /// the current view does not show — `model_code` here (`schema()`'s
    /// carried dimension, `grain = "instrument"`) is exactly that shape.
    /// `delta01`/`daily_trading_pnl` are measures and must never appear
    /// for `group`.
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

    /// 2c §6.2: the definitions travel from the factory's shared cell to
    /// the delegate, and they travel on the plan — the tile hands them
    /// over in `apply`, where the plan is built, so the delegate can
    /// never be painting a plan against colours older than it. Asserted
    /// through `cell_colour`, the same door `render_td`/`render_th` use.
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
            Some(crate::colour_cache::Resolved::plain(
                geode_shell::shell::colours::to_hsla(danger)
            )),
            "the tile's own colours must reach the delegate with the plan"
        );
    }

    /// The rule (command-line locality spec §2): a `:` line changes only
    /// this tile. Every word the parser accepts — with a valid argument
    /// where one is needed — plus every refusal, runs against a tile
    /// while the frame's scope/grouping/as-of counters, its slot set and
    /// its pending slot persist are watched. `COMMANDS` is read so a word
    /// added there without a line here fails.
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
                    f.versions().scope,
                    f.versions().grouping,
                    f.versions().as_of,
                    f.slots().clone(),
                    f.scope().clone(),
                    f.as_of().clone(),
                )
            })
        };
        let before = read(&cx);
        for line in lines {
            let _ = h.tile.update(&mut cx, |t, cx| t.command(line, cx));
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
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
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
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("new scope".into()));
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now());
            cx.notify();
        });
        let _a = next_query(&h.requests);
        let b = next_query(&h.requests);
        publish_for(&frame, &mut vcx, "unrelated");
        assert!(h.requests.try_recv().is_err());
        assert!(
            frame.read_with(&vcx, |f, _| f.barrier_wants(QueryKey(8), f.versions())),
            "query still outstanding"
        );
        deliver_to(&h.b, QueryKey(8), &mut vcx, b.tag, Ok(snapshot2()));
        assert!(h.b.read_with(&vcx, |t, _| t.staged.is_some()));
        publish_for(&frame, &mut vcx, "unrelated");
        assert!(h.requests.try_recv().is_err());
        assert!(
            frame.read_with(&vcx, |f, _| f.barrier_open()),
            "A is still outstanding"
        );
        frame.update(&mut vcx, |f, cx| {
            f.arrived(QueryKey(7), f.versions());
            cx.notify();
        });
        assert_eq!(
            shown_texts(&h.b, &vcx),
            vec!["", "M1", "M2"],
            "unrelated data must not discard a valid staged result"
        );
    }
}
