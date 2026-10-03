//! The shell's window root: owns key dispatch, workspace state, overlays,
//! and tile occupants. The toolbar serves as the native title bar; the
//! sidebar and status bar surround the remaining tile area. Rendering,
//! input routing, persistence, and occupant lifecycle live in sibling modules.

mod add_tile;
pub mod addfilter;
pub mod aggregates;
pub mod asof_rows;
pub mod asof_view;
pub mod chip;
pub mod choicedialog;
pub mod colours;
mod commandline_ctl;
pub mod commandline_view;
pub mod control;
pub mod dialog;
mod drag;
pub(crate) mod expr_suggest;
mod hot_reload;
mod input;
pub mod kbd;
pub mod keybindings_view;
pub mod keys;
mod link;
pub mod listrow;
pub mod objectdialog;
mod occupants;
mod page;
mod palette_ctl;
pub mod perf_overlay;
pub mod picker;
mod pin;
#[cfg(feature = "profiling")]
pub mod profiling_hook;
mod render;
pub mod row_menu;
mod rows;
pub mod scale;
pub mod scope_expr_view;
mod session_io;
pub mod settings_view;
pub mod sidebar;
pub mod stacklist;
pub mod status;
pub mod toolbar;
pub mod whichkey;

pub use keys::convert_keystroke;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use gpui::prelude::*;
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable as _, ScrollHandle, SharedString,
    UniformListScrollHandle, Window,
};
use gpui_component::input::{InputEvent, InputState};

use crate::actions::ActionRegistry;
use crate::commandline::CommandLine;
use crate::diagnostics::{ActionTail, Diagnostics};
use crate::fontsize::FontSize;
use crate::frame::{Frame, FrameRef, FrameVersions};
use crate::keymap::{Keymap, Matcher, Modifiers};
use crate::log_persist;
use crate::module::{ModuleRoster, PageRoster, TileOccupant};
use crate::palette::PaletteState;
use crate::perf::FrameHistogram;
use crate::reload;
use crate::session;
use crate::theme::ThemeService;
use crate::tiling::{TileId, WorkspaceIx, Workspaces};
use crate::vimfind::FindStyle;
use geode_core::config::{Config, ConfigSources, Diagnostic, LayerDoc};
use geode_core::dimensions::DerivedDimensions;
use geode_core::log::{LevelControl, LogLevels, Ring};
use geode_core::query::{DistinctOutcome, QueryKey};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::sync::{Arc, Mutex};

/// Everything the shell needs to run a window, assembled once by the app
/// from loaded config, the action registry, the compiled keymap, and the
/// initial workspace state. `ShellView` owns this for the
/// life of the window.
pub struct ShellServices {
    pub config: Config,
    /// The compiled-in builtin layer used to load `config`. Hot reload reads
    /// only the desk and user directories and reuses these exact documents,
    /// including any demo datasets, sources, and views supplied by the app.
    pub builtin: Vec<LayerDoc>,
    pub registry: ActionRegistry,
    pub keymap: Keymap,
    pub mod_alias: Modifiers,
    pub workspaces: Workspaces,
    pub theme: ThemeService,
    /// Session file used by periodic and shutdown saves. `None` disables
    /// session persistence; layout actions only mark pending state dirty.
    pub session_path: Option<PathBuf>,
    /// The modules the app compiled in; the shell creates tile
    /// occupants through it and never names a module crate.
    pub roster: ModuleRoster,
    /// Module names and state from `session.toml`, consumed as the shell
    /// creates occupants. Unavailable kinds retain their records separately.
    pub restored_tiles: crate::session::TileRecords,
    /// Optional scope, grouping slot, and as-of from `[frame]`.
    /// `ShellView::new` applies them and clears scope undo/redo history.
    pub restored_frame: Option<crate::session::FrameRecord>,
    /// Pinned workspace lanes from `session.toml`'s `workspaces.N.frame`;
    /// `ShellView::new` pins each workspace and fills its lane with clean
    /// scope history.
    pub restored_pinned: crate::session::PinnedRecords,
    /// Link group scopes from `session.toml`'s `[links.<letter>]`, in
    /// `Group::ALL` order; `ShellView::new` sets each non-empty one on its
    /// group before any tile is linked or created.
    pub restored_links: crate::session::GroupScopes,
    /// The palette's usage history from `session.toml`'s `[palette.usage]`
    /// table — empty for a fresh session and in every test setup that
    /// doesn't opt in. `ShellView::new` takes it as the live history.
    pub restored_palette_usage: crate::palette_usage::PaletteUsage,
    /// Optional logging services: the diagnostics ring, runtime level control,
    /// and startup levels. The palette updates levels through this control.
    /// When absent, the shell skips logging integration.
    pub log: Option<LogServices>,
    /// The last 32 dispatched actions' hashes: recorded
    /// by `ShellView::dispatch` before it matches the action, read by the
    /// crash hook (`geode_app::crash::install_panic_hook`) through the
    /// `Arc<Mutex<_>>` handed to it at startup — a shared handle, not a
    /// snapshot, so the hook always sees the latest keypresses right up
    /// to the panic. A `Mutex`, not `RefCell`: this must be `Send + Sync`
    /// to be captured by the 'static panic hook closure alongside
    /// `ActionRegistry::hash_names`'s own `Arc<RwLock<_>>`.
    pub action_tail: Arc<Mutex<ActionTail>>,
    /// Startup keymap diagnostics, resolved against the app's action registry.
    /// Carried separately because `Config` alone cannot reproduce them. The
    /// shell includes them in the diagnostics entity from the first frame.
    pub keymap_diagnostics: Vec<Diagnostic>,
    /// Validated module keymap fragments produced by `ModuleRoster` at startup.
    /// Hot reload splices these same fragments back into the loaded config.
    /// Reusing them preserves module bindings without revalidating and logging
    /// compiled-in fragment errors on every reload.
    pub keymap_fragments: Vec<LayerDoc>,
    /// Diagnostics from validating compiled-in module fragments, including
    /// undeclared contexts and parse failures. Reload includes these after
    /// `reload::decide`: a module fragment error must remain visible without
    /// rejecting the user's config. The dropped bindings cannot be diagnosed
    /// again by `build_keymap`. Startup also includes this list in
    /// [`Self::keymap_diagnostics`].
    pub keymap_fragment_diagnostics: Vec<Diagnostic>,
    /// Diagnostics from startup-only composition the shell cannot recompute,
    /// such as market-data panels refused at startup. Seeded into the config
    /// section and restated after every reload: only a restart changes which
    /// panels exist, so a fix on disk shows `restart required` beside the
    /// standing error until then.
    pub composition_diagnostics: Vec<Diagnostic>,
    /// The app's registered pages, in sidebar order. Empty in tests that
    /// build no page.
    pub pages: PageRoster,
    /// `[pages.<kind>]` tables from the loaded session, consumed by the
    /// page's first open. Unmatched tables are carried to the next save.
    pub restored_pages: std::collections::BTreeMap<String, toml::Table>,
}

/// Runtime logging services: the diagnostics ring, the palette's level
/// control, and the currently effective levels.
pub struct LogServices {
    pub ring: Arc<Ring>,
    pub control: Arc<dyn LevelControl>,
    pub levels: LogLevels,
}

impl ShellServices {
    /// Load config and retain the exact builtin documents used for that load.
    /// Constructing both from one `ConfigSources` value keeps startup and
    /// reload consistent. The caller assembles the remaining services, many of
    /// which depend on the resulting config.
    pub fn config_and_builtin(sources: ConfigSources) -> (Config, Vec<LayerDoc>) {
        let builtin = sources.builtin.clone();
        (Config::load(&sources), builtin)
    }
}

/// Events for the app bridge: configuration changes and distinct-value
/// requests. The bridge submits data requests and delivers their outcomes;
/// the shell does not depend on `geode-data`.
///
/// `PartialEq` follows `DistinctParams`: its scope can contain floating-point
/// literals and therefore does not implement `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    /// Views, dimensions or groupings changed and were applied; the app
    /// bridge forwards the new views to the data thread.
    ConfigReloaded,
    /// The `app` document changed and was applied. Queued, like
    /// `ConfigReloaded`, before the frame's revision notification, so a
    /// module setting the app bridge reads from it (`blotter.stale_after`)
    /// reaches the factories before their tiles observe the reload.
    AppSettingsReloaded,
    /// Restart-sensitive configuration differs from the running data engine:
    /// sources, datasets, egress targets, the position service, panels, the
    /// pricing adapter or the vol model. Presentation changes such as
    /// grouping labels can still apply immediately.
    RestartRequired(String),
    /// A dimension picker (`shell::picker`) needs distinct values for one
    /// column, scoped by everything except that column's own selection
    /// — the caller has already done that removal. The bridge
    /// calls `handle.distinct(params)`; the result comes back as
    /// `DataEvent::Distinct`, which the bridge routes to
    /// [`ShellView::deliver_distinct`].
    DistinctRequested(geode_core::query::DistinctParams),
    /// The last reload was refused — `reload::decide` kept the previous
    /// config because the new one carried these error diagnostics.
    /// Distinct from `RestartRequired`: nothing here is live, the file is
    /// on disk exactly as written, and a dialog that just wrote it needs
    /// to say so. Carries the diagnostics, not the count, so a consumer
    /// can name the file.
    ReloadRejected(Vec<geode_core::config::Diagnostic>),
}

impl EventEmitter<ShellEvent> for ShellView {}

/// The coalescing key the dimension pickers submit their `Request::
/// Distinct` under. Reserved, not user-reachable: every real
/// tile's query key comes from `TileId`, which is a small
/// sequential counter nowhere near `u64::MAX`, so this can never collide
/// with a live tile. `ShellView::deliver` (the `Delivery` route) never
/// sees this key — a picker's own outcome arrives as `DataEvent::Distinct`
/// instead and is routed to [`ShellView::deliver_distinct`], a separate
/// method with its own stale-tag/stale-column guard.
pub const PICKER_KEY: QueryKey = QueryKey(u64::MAX - 1);

/// The coalescing key the diagnostics page's `Request::Catalog` submits
/// under — same reservation reasoning as [`PICKER_KEY`]
/// just above, one lower so the two can never collide with each other or
/// with a real tile's `TileId`-derived key.
pub const DIAGNOSTICS_KEY: QueryKey = QueryKey(u64::MAX - 2);

/// The Scopes dialog's Values stage submits its `Request::Distinct` under
/// this key — one lower than `DIAGNOSTICS_KEY`,
/// same reservation reasoning. `deliver_distinct` routes on it.
pub const SCOPES_KEY: QueryKey = QueryKey(u64::MAX - 3);

/// The scope expression suggestions' distinct-values requests (both the
/// frame dialog and the Scopes dialog's `expression` field). Routed by
/// [`ShellView::deliver_distinct`] to `expr_suggest::deliver`, which drops
/// any reply whose tag is not its column's latest.
pub const EXPR_KEY: QueryKey = QueryKey(u64::MAX - 4);

/// A row action's value choice (`ActionCx::choose_value`) submits its
/// `Request::Distinct` under this key — one lower than `EXPR_KEY`, same
/// reservation reasoning. [`ShellView::deliver_distinct`] routes it to the
/// open choice dialog, which drops a reply whose tag is not its own.
pub const ACTION_KEY: QueryKey = QueryKey(u64::MAX - 5);

/// The bridge's live reference reads (`Request::Reference` at `AsOf::Live`)
/// submit under this key — one lower than `ACTION_KEY`, same reservation
/// reasoning. Their answers never reach the shell's delivery routes: the
/// bridge keeps them in [`crate::reference::ReferenceGlobal`], dropping any
/// whose tag is not its dataset's latest.
pub const REFERENCE_KEY: QueryKey = QueryKey(u64::MAX - 6);

/// One column a dimension picker can open: every categorical
/// column of every dataset, plus every derived dimension. `role` is
/// `"dimension"` for a real `ColumnRole::Dimension` column, `"attribute"`
/// for any other categorical column (an attribute that opted in via
/// `categorical = true`), and `"derived"` for a `[dimensions]` entry —
/// see [`pickable_columns`]. `datasets` lists every dataset the column
/// appears in (first-seen order, appended as more datasets carry it);
/// empty for a derived dimension, which is desk config rather than a real
/// dataset column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickable {
    pub column: String,
    pub role: &'static str,
    pub datasets: Vec<String>,
}

/// Collect categorical dataset columns in schema order, merging repeated
/// column names across datasets and appending their dataset names. Append
/// derived dimensions afterward with role `"derived"`. Key columns are not
/// categorical by default, so ordinary keys are excluded.
///
/// The shell caches this at construction and rebuilds it when datasets or
/// derived dimensions change.
pub fn pickable_columns(config: &Config) -> Vec<Pickable> {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();

    let mut out: Vec<Pickable> = Vec::new();
    // A computed dataset's values cannot be listed (no relation); its
    // columns that another dataset shares are picked through that dataset.
    for dataset in schema.datasets.iter().filter(|d| !d.computed) {
        for column in dataset.categorical_columns() {
            if let Some(p) = out.iter_mut().find(|p| p.column == column) {
                p.datasets.push(dataset.name.clone());
                continue;
            }
            let role = match dataset.column(column).map(|c| c.role) {
                Some(ColumnRole::Dimension { .. }) => "dimension",
                _ => "attribute",
            };
            out.push(Pickable {
                column: column.to_string(),
                role,
                datasets: vec![dataset.name.clone()],
            });
        }
    }
    for dim in dims.all() {
        out.push(Pickable {
            column: dim.name.clone(),
            role: "derived",
            datasets: Vec::new(),
        });
    }
    out
}

/// Every column a scope expression may name, for the expression
/// suggestions and their schema check. Cached on the shell and rebuilt
/// when datasets or dimensions reload.
pub fn expr_vocab(config: &Config) -> geode_core::scope::complete::ExprVocab {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    geode_core::scope::complete::ExprVocab::new(&schema, &dims)
}

/// Every column a grouping slot may name, in schema order — the query
/// compiler's own vocabulary (`carries_all` in `geode-data`'s `compile`),
/// not the picker's: for each dataset, [`geode_core::schema::DatasetSpec::groupable_columns`]
/// — every declared column some *declared* grain carries as a dimension:
/// a grain's key columns (`position_ref`, `instrument_ref`) and every
/// carried dimension, categorical or not (a numeric strike included) —
/// then every derived dimension whose base column (`from`) is itself in
/// that set, role `"derived"`, since the compiler resolves a derived
/// dimension through `dims.base_column` and refuses one over a column no
/// grain carries. `role` is `"key"` for a `ColumnRole::Key` column and
/// `"dimension"` otherwise. First-seen order across datasets, exactly as
/// [`pickable_columns`].
///
/// This is deliberately neither a superset nor a subset of the picker's
/// list: a categorical attribute is pickable (its ENUM dictionary is
/// something to browse) but no grain carries it as a dimension, so a
/// slot naming it would fail to compile; a key column is the reverse,
/// groupable but with no dictionary to pick from. The Groupings dialog
/// (`objectdialog::groupings::fields`) reads this one.
pub fn groupable_columns(config: &Config) -> Vec<Pickable> {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();

    let mut out: Vec<Pickable> = Vec::new();
    for dataset in &schema.datasets {
        for column in dataset.groupable_columns() {
            if let Some(p) = out.iter_mut().find(|p| p.column == column) {
                p.datasets.push(dataset.name.clone());
                continue;
            }
            let role = match dataset.column(column).map(|c| c.role) {
                Some(ColumnRole::Key) => "key",
                _ => "dimension",
            };
            out.push(Pickable {
                column: column.to_string(),
                role,
                datasets: vec![dataset.name.clone()],
            });
        }
    }
    let base_groupable = |from: &str| out.iter().any(|p| p.column == from);
    let derived: Vec<Pickable> = dims
        .all()
        .filter(|dim| base_groupable(&dim.from))
        .map(|dim| Pickable {
            column: dim.name.clone(),
            role: "derived",
            datasets: Vec::new(),
        })
        .collect();
    out.extend(derived);
    out
}

/// Load and validate saved scopes against the configured datasets and
/// derived dimensions. Shared by startup action registration and hot reload;
/// the caller controls whether validation diagnostics are logged.
pub use hot_reload::rebuild_saved_scopes as saved_scopes;

/// One addressed occupant request.
pub(super) struct PendingTile {
    pub(super) kind: String,
    /// The record the factory sees as `restored` — a duplicate's
    /// serialized state, `None` for a plain add.
    pub(super) state: Option<toml::Table>,
}

/// Window root that routes keyboard input through the layered,
/// sequence-aware [`Matcher`]. Rendering and modal input handling share
/// this state with the workspace and tile lifecycle.
///, not a static `KeyBinding` table.
pub struct ShellView {
    services: ShellServices,
    matcher: Matcher,
    /// The effective UI font size (small/medium/large — `[ui] font_size`).
    /// Applied as the window's rem size at the top of `render`, the one
    /// place with a `Window` on every path that can change it (startup,
    /// the settings control via `settings_view::set_font_size`, config hot
    /// reload) — see the `fontsize` module doc.
    font_size: FontSize,
    /// The configured `/`-find style (`[ui] find_style`). Resolved at startup,
    /// on reload, and by the settings control. Each new `/` prompt chooses
    /// the tile's Vim event stream or the shared Fzf picker from this value.
    find_style: FindStyle,
    focus_handle: FocusHandle,
    /// State for the open command palette, or `None`. Each open rebuilds the
    /// registry/keymap/theme snapshot and reverse binding index; closing drops
    /// it. Usage history persists separately in `palette_usage`.
    palette: Option<PaletteState>,
    /// How often and how recently each palette row was chosen
    /// (`crate::palette_usage`): read once per palette open to rank the
    /// rows (`PaletteState::with_usage`), written by every palette
    /// dispatch but `palette::toggle`'s own row, and persisted as
    /// `session.toml`'s `[palette.usage]` table through the same
    /// coalesced flush the layout rides.
    palette_usage: crate::palette_usage::PaletteUsage,
    /// Bumped by every `palette_usage` mutation; `take_dirty_session_write`
    /// compares it against `last_palette_usage_written`, the same shape
    /// as `last_frame_generation_written`, so a palette dispatch that
    /// mutates nothing else still reaches the flush.
    palette_usage_version: u64,
    last_palette_usage_written: u64,
    /// Open modals, bottom first. Only the last entry paints and receives keys;
    /// the rest keep their state and reappear when everything above them pops.
    /// Installed through `dialog::open_shell_dialog`. Dialog-specific data and
    /// scroll handles live in the per-kind fields below; a kind's field is `Some`
    /// exactly while that kind is in this stack.
    modals: Vec<dialog::ShellModal>,
    /// State for the open keybinding dialog. Created by `keybindings_view::open`
    /// and cleared when its kind pops. The render callback and modal handler share this
    /// state; GPUI scrolling remains in `keybindings_scroll` so filtering,
    /// selection, and capture can be tested without a window.
    keybindings: Option<keybindings_view::KeybindingsState>,
    /// Scroll state for the open keybinding dialog's row list — same
    /// reasoning and lifecycle as `palette_scroll` (a fresh `ScrollHandle`
    /// per open, driven by `keybindings_view`'s selection-change paths via
    /// `ScrollHandle::scroll_to_item`).
    keybindings_scroll: ScrollHandle,
    /// State for the open settings dialog, created by [`settings_view::open`]
    /// and cleared when its kind pops. Selection, filtering, and choice editing remain
    /// independent of GPUI; scrolling lives in `settings_scroll`.
    settings: Option<settings_view::SettingsState>,
    /// Scroll state for the open settings dialog's row list — the
    /// `keybindings_scroll` split, one dialog over.
    settings_scroll: ScrollHandle,
    /// Stable scroll handle for the open palette's virtualized list. Each open
    /// creates a new handle; every render tracks that same handle. Selection
    /// changes call `sync_palette_scroll` to keep the highlighted row visible.
    palette_scroll: UniformListScrollHandle,
    /// The palette query input, built once so its focus handle, selection, and
    /// change subscription survive close/reopen. Opening the palette resets its
    /// value and focuses it; closing returns focus through
    /// `return_focus_from_overlay`.
    ///
    /// The input handles text editing and paste. Single-line navigation keys
    /// (up/down, ctrl+p/ctrl+n), enter, and escape reach the palette key handler.
    /// Other keys must keep propagating so IME/text-input dispatch can deliver
    /// text to this field.
    palette_input: Entity<InputState>,
    /// The dialogs' shared input. Only the top of the modal stack owns it;
    /// popping one level clears only that kind's own state and hands the
    /// input back to the entry beneath, restoring its saved text and caret
    /// (see `dialog::refocus_top`). The entity and focus handle persist;
    /// each dialog resets its value on open.
    ///
    /// Normal mode blurs the input so letters reach dialog commands. Entering
    /// filter mode focuses it; leaving filter mode blurs it again. Keybinding
    /// capture also blurs it and restores the starting mode's focus on exit.
    /// Value and naming fields manage focus according to their own editing state.
    dialog_input: Entity<InputState>,
    /// Config directories read by each watcher poll from the live shell state.
    desk_dir: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    /// Last config-directory snapshot. The first background scan replaces the
    /// empty initial value without triggering a reload. Later scans compare
    /// against it about every 500 ms. Scanning never runs on the UI thread.
    last_snapshot: reload::Snapshot,
    /// The result of the last reload attempt, `Unchanged` until the first
    /// one runs. Drives the status bar's reload indicator.
    last_reload: reload::ReloadOutcome,
    /// The rejected-reload segment, prepared when `last_reload` is set.
    pub(crate) reload_status: Option<SharedString>,
    /// Bumped by every applied reload — the one place `services.config` and
    /// `services.keymap` are replaced. Config dialog rows derived from either are
    /// keyed by it (`crate::prepared`).
    config_revision: u64,
    /// Layout changes awaiting snapshot extraction. The periodic watcher
    /// clears this flag before serialization and writes in the background;
    /// it is not a record of whether the disk write succeeded.
    session_dirty: bool,
    /// Tile records from the last successfully serialized periodic snapshot,
    /// initially empty. Compared every tick to catch module-only state changes.
    /// Updated before disk I/O; a failed write does not reset this baseline.
    last_tiles_written: crate::session::TileRecords,
    /// Page records from the last successfully serialized periodic snapshot,
    /// initially empty. Compared every tick like `last_tiles_written`, since
    /// a page's state changes do not set the layout flag.
    last_pages_written: crate::session::PageRecords,
    /// The frame's generation counter captured by the last successfully
    /// serialized periodic snapshot, initially zero. Every scope, grouping,
    /// as-of, pin, and unpin change in any lane advances it, so it detects
    /// frame-only changes; updated before the disk write completes.
    last_frame_generation_written: u64,
    /// The last periodic snapshot handed to the writer, as text WITHOUT the
    /// link groups' scopes (`[links]`); `None` before the first. A group's
    /// scope moves with an emitting tile's cursor and advances the frame
    /// generation, so a dirty check can produce a snapshot that differs
    /// from the last only there; that snapshot is not written. The file's
    /// text carries the groups' scopes whenever a snapshot is written.
    last_session_text: Option<String>,
    /// Deferred focus restoration for paths without a `Window`, such as hot
    /// reload closing the palette. `render` consumes it before painting. Waiting
    /// for a key event is unsafe: dropping a focused overlay can leave no live
    /// focus target through which that event could reach the shell.
    pending_focus_restore: bool,
    /// Set when a tile's × closed it; the shell root's capture-phase press
    /// listener swallows the rest of that double-click (any press with
    /// `click_count > 1`) and the next first press clears it. Closing
    /// changes what lies under the pointer — a neighbour's ×, a focused
    /// placeholder, the empty tree, a module's header — and each has its
    /// own double-click gesture the follow-on press must not reach.
    swallow_double_click_followup: bool,
    /// Whether the scope input held focus when an overlay opened. Closing the
    /// overlay consumes this flag and restores either the input or shell focus.
    /// Palette selection closes before dispatching, so a dialog launched from
    /// the palette records the restored focus itself. One flag suffices: it
    /// belongs to the first overlay opened, and a palette or dialog opened over
    /// an open dialog neither records nor consumes it.
    overlay_return_to_filter: bool,
    /// Active divider drag. Mouse moves resize the layout immediately; release
    /// or cancellation preserves those changes and marks the session dirty if
    /// it moved. Opening an overlay, changing workspace, or entering fullscreen
    /// cancels tracking because the dragged boundary is no longer reachable.
    divider_drag: Option<drag::DividerDrag>,
    /// Active mod+drag of a tile. Movement updates the drop preview; release
    /// applies the selected workspace drop operation. Cancellation changes no
    /// layout or persistence state. Overlays, workspace changes, fullscreen,
    /// and which-key hints cancel tracking when they obscure its targets.
    tile_drag: Option<drag::TileDrag>,
    /// The per-tile command line's input, built once like
    /// `palette_input` — a stable `Entity<InputState>` across frames, its
    /// value reset (not rebuilt) on every open.
    command_input: Entity<InputState>,
    /// The open command line's own pure state, or `None` when
    /// closed. Set fresh by `open_command_line` each time (mirrors
    /// `palette`'s "nothing survives a close/reopen" contract) and read/
    /// mutated by `handle_command_line_key`/`on_command_line_changed` and
    /// painted by `commandline_view::render`.
    command_line: Option<CommandLine>,
    /// Completion viewport, retained across renders and reset on each prompt open.
    command_scroll: ScrollHandle,
    fuzzy_find: Option<Entity<crate::fuzzyfind::FuzzyFind>>,
    fuzzy_find_subscriptions: Vec<gpui::Subscription>,
    /// The scope bar's text input. Each focused edit updates the frame inside
    /// one undo session. The stable entity preserves cursor, selection, and
    /// focus across renders.
    filter_input: Entity<InputState>,
    /// The field's value at the moment it took focus,
    /// captured by the `InputEvent::Focus` arm of `filter_input`'s
    /// subscription and taken by whichever of Enter/Escape/Blur ends the
    /// session first. `Some` only while a text-editing session is open —
    /// `handle_key_down`'s escape branch uses it to restore the pre-focus
    /// text; `Enter`/`Blur` just clear it without restoring anything.
    filter_session_base: Option<String>,
    /// Always-on render-interval histogram. Recording mutates a fixed array
    /// without locks, allocation, or scheduling another frame. See `crate::perf`
    /// for the limits of this signal.
    perf: FrameHistogram,
    /// `Instant` at the top of the previous `render` call, the other half
    /// of the frame-interval measurement. `None` until the first render
    /// (nothing to measure yet) — never reset after that: an idle gap is
    /// excluded by `perf::IDLE_CUTOFF` at record time instead.
    last_render_started: Option<std::time::Instant>,
    /// Whether the perf readout overlay is painted (`perf::toggle_overlay`,
    /// palette-reachable, bound `mod+shift+p`). Display-only: toggling it
    /// changes nothing about recording, which always runs.
    perf_overlay: bool,
    /// The one page this window has created, open or not. Created on its
    /// first `page::toggle_<kind>` and retained so a round trip keeps its
    /// state; `open` is the only flag a toggle changes. `None` until then.
    page: Option<page::OpenPage>,
    /// The registered pages' sidebar entries, collected once at construction:
    /// the roster never changes after startup, and the sidebar paints from
    /// this on every render without collecting.
    page_entries: Vec<crate::module::PageEntry>,
    /// The shared frame, created here so every occupant can hold it.
    frame: Entity<Frame>,
    /// Shared diagnostics state, fed by the app bridge and config load/reload.
    /// Its observer drains log-level and overlay requests; the app bridge
    /// handles catalog requests.
    diagnostics: Entity<Diagnostics>,
    /// Pending occupant creation, addressed by the tile ID allocated or filled
    /// by `add_tile`. `ensure_occupants` consumes each request at that tile on
    /// the next render; simultaneous requests cannot replace one another.
    pending_tiles: BTreeMap<TileId, PendingTile>,
    /// Restored records whose module factory is unavailable. `current_tiles`
    /// writes them back unchanged, preserving sessions from builds with more
    /// modules. Closing or filling their tile removes them.
    unplaced_records: crate::session::TileRecords,
    /// `[tiles] add`: resolved at startup,
    /// re-derived on hot reload, stepped by the settings row.
    pub(super) add_direction: crate::tileadd::AddDirection,
    /// `[ui] line_numbers`: same lifecycle as
    /// `add_direction`, and additionally published as the
    /// `linenumbers::UiSettings` global on every change so a module
    /// (the blotter) can read and observe it — see that module's doc.
    pub(super) line_numbers: crate::linenumbers::LineNumbers,
    /// `[timeseries] default_source`: same
    /// lifecycle as `line_numbers`, published with it as the
    /// `series::SeriesSettings` global so a timeseries tile can read the
    /// source `:add` means without a path to `ShellView`.
    pub(super) default_source: Option<String>,
    /// The frame's `(scope, grouping, as_of)` versions as of the last
    /// `on_frame_changed` — compared against the frame's
    /// current ones there to decide whether to open a fresh flip barrier.
    /// Seeded once in `new`, right after a restored session's scope/slot/
    /// as-of are applied, so that restore is never itself mistaken for
    /// "just changed" (which would open a barrier over whatever tiles
    /// happen to exist yet at construction time, before any occupant
    /// does).
    last_flip_versions: FrameVersions,
    /// Each link group's scope generation as of the last `on_frame_changed`,
    /// in `Group::ALL` order: the other half of the flip baseline. A group
    /// whose number moved flips its visible followers. Seeded beside
    /// `last_flip_versions` in `new`, so a restored group scope is not read
    /// as a change. Not re-seeded on a workspace switch: the numbers are
    /// frame-wide, so a switch changes none of them, and re-seeding there
    /// would swallow a group change whose notification is still pending.
    last_flip_groups: [u64; 4],
    /// Who lives in each tile. Created lazily in `ensure_occupants` and
    /// dropped when the tile is gone from every workspace.
    occupants: HashMap<TileId, TileOccupant>,
    /// The tiles painted last frame, to diff visibility without touching
    /// every occupant every frame.
    visible_tiles: HashSet<TileId>,
    /// One subscription per tile that emits into a link group, held exactly
    /// while it does: the tile's `watch_emission` calls back through it and
    /// the shell pulls. Dropped when the tile leaves its group or closes,
    /// so a tile in no group is never pulled.
    emit_subs: HashMap<TileId, gpui::Subscription>,
    /// The status bar's `following` label and what it was built from: the
    /// focused tile, the group it follows and that group's scope
    /// generation. Refreshed in render preparation (`refresh_link_label`)
    /// and rebuilt only when that key changes, so a repaint formats
    /// nothing. `None` while the focused tile follows no group.
    link_label: Option<(link::LinkLabelKey, gpui::SharedString)>,
    /// The last `(index, len)` `ensure_occupants` delivered to each tile
    /// through `TileContent::set_stack` — a
    /// missing entry means "unsent", so a fresh occupant always hears its
    /// stack position once (`None` included) and a later render tells it
    /// again only when the value actually changes. Retained to live tiles
    /// at the end of every `ensure_occupants`, the same lifecycle
    /// `pending_tiles`/`unplaced_records` follow.
    stack_sent: HashMap<TileId, Option<(usize, usize)>>,
    /// The tile last told it is focused (`TileContent::set_focused`), so
    /// focus is diffed rather than sent to every occupant every frame.
    focused_sent: Option<TileId>,
    /// Status notice, such as a refused stack action's `"not in a stack"`
    /// or a row menu action's report (`opened <url>`).
    /// Cleared at the start of the next dispatch.
    notice: Option<SharedString>,
    /// The transient stack-member list, or
    /// `None` when closed — `open_stack_list`'s own contract, the same
    /// "nothing survives a close/reopen" shape `palette`/`command_line`
    /// follow. Owns the keyboard while open (`handle_key_down`'s own
    /// branch), closed by any dispatch (`dispatch`'s own top, beside
    /// `notice`), and dropped by `render`'s generic staleness check when
    /// its tile stops being the focused member.
    stack_list: Option<stacklist::StackList>,
    /// The scope bar's open "Add a filter" menu, or `None` when closed.
    /// Owns the keyboard while open (`handle_key_down`'s own branch),
    /// closed by any dispatch (which is also how a row commits), by the
    /// palette or a dialog opening, and by a click outside it.
    add_filter_menu: Option<addfilter::AddFilterMenu>,
    /// The row menu (`tile::context_menu`), or `None` when closed. Owns
    /// the keyboard while open, as `add_filter_menu` does, and closes the
    /// same ways: any dispatch, the palette or a dialog opening, a press
    /// outside it.
    row_menu: Option<row_menu::RowMenu>,
    /// Reusable storage for the per-frame tile diff. Each reconciliation
    /// clears and refills it, retaining capacity between renders.
    scratch_all_tiles: HashSet<TileId>,
    /// Same purpose as `scratch_all_tiles`, for the active-tiles half of
    /// the diff.
    scratch_active_tiles: HashSet<TileId>,
    /// Scratch storage for `visible_tile_keys`, same
    /// reasoning as the two fields above — a flip only opens on a user
    /// mutation of scope/grouping/as-of, nowhere near every render, but
    /// there is no reason for it to allocate fresh every time either.
    scratch_visible_keys: Vec<QueryKey>,
    /// Restart notice for config that differs from the running data service.
    /// Reload compares restart-sensitive settings against their startup
    /// baselines; restoring those values clears the notice. The status bar
    /// paints it and the app bridge receives `ShellEvent::RestartRequired`.
    restart_required: Option<SharedString>,
    /// The `sources` layered doc the running `DataService` was actually
    /// built from — captured once here at construction, since a reload
    /// never rebuilds the data engine (see `apply_reload`'s doc comment).
    /// `apply_reload` compares each freshly loaded config's `sources` doc
    /// against this baseline, not against the previous reload's config,
    /// so reverting an edit back to the value the service was built from
    /// clears `restart_required`: the message means "what's on disk no
    /// longer matches what's running", and a revert makes that false
    /// again.
    sources_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `datasets` doc.
    datasets_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `egress` doc: nothing reloads a
    /// resolved target's transport live, so `egress.toml` is restart-
    /// required exactly as `sources.toml` is.
    egress_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `positions` doc: the position service is resolved once at
    /// startup, so `positions.toml` is restart-required exactly as
    /// `egress.toml` is.
    positions_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `panels` doc: panels become tile kinds once at startup, so
    /// `panels.toml` is restart-required.
    panels_baseline: Vec<LayerDoc>,
    /// The startup `[pricing] adapter` value used to build the data engine.
    /// Reload compares against this baseline to determine whether a restart is
    /// required. The rest of `[pricing]`, including `refresh`, remains live.
    pricing_baseline: Option<toml::Value>,
    /// The startup `[vol] model` value used to build the data engine's vol
    /// worker, compared on reload exactly as `pricing_baseline` is.
    vol_baseline: Option<toml::Value>,
    /// Cached [`pickable_columns`] for the current datasets and dimensions.
    /// Reload rebuilds this list; startup registers per-column actions once.
    /// Columns added later remain reachable through the two-stage `frame::pick`
    /// flow even though they have no new per-column palette action.
    pickable: Vec<Pickable>,
    /// Cached [`expr_vocab`] for the current datasets and dimensions: the
    /// scope expression suggestions' columns. Reload rebuilds it beside
    /// `pickable` and re-ranks an open expression field against it.
    expr_vocab: std::rc::Rc<geode_core::scope::complete::ExprVocab>,
    /// State for the open dimension picker. Created by `picker::open` and
    /// cleared when its kind pops.
    picker: Option<picker::PickerState>,
    /// Next distinct-request tag. Shared across picker opens so a late reply
    /// from a closed picker cannot match a fresh request for the same column.
    next_picker_tag: u64,
    /// Scroll handle for the picker's virtualized Values list. Selection-change
    /// paths call `picker::sync_picker_scroll` to keep keyboard navigation
    /// visible; virtualization alone does not move the viewport.
    picker_scroll: UniformListScrollHandle,
    /// State for the open as-of dialog. Created by [`asof_view::open`] and
    /// cleared when its kind pops.
    as_of_dialog: Option<asof_rows::AsOfState>,
    /// Scroll handle for the as-of rows. Navigation and refresh use
    /// `asof_rows::child_index_of` to keep the highlighted row visible.
    as_of_scroll: ScrollHandle,
    /// The frame data version used to build the open as-of dialog's rows.
    /// `on_frame_changed` refreshes them only when this version changes. Set on
    /// open and unused while the dialog is closed.
    as_of_data_version: u64,
    /// State for the open scope expression dialog. Created by
    /// [`scope_expr_view::open`] and cleared when its kind pops.
    scope_expr_dialog: Option<scope_expr_view::ScopeExprState>,
    /// State for the open choice dialog: the scope, tile-kind, column,
    /// log-level, action-value or link-group picker. Created by one of
    /// `choicedialog`'s `open_*` doors, and cleared when its kind pops.
    choice_dialog: Option<choicedialog::ChoiceDialogState>,
    /// Scroll state for the choice dialog's row list
    /// (`dialog::choice_rows`'s viewport) — the `settings_scroll` split,
    /// one dialog over.
    choice_dialog_scroll: ScrollHandle,
    /// Scroll state for the scope expression suggestions' row list. Refresh
    /// scrolls it to the top; a highlight move follows the lit row.
    expr_scroll: ScrollHandle,
    /// State for the open config-object dialog, shared across config domains.
    /// Created by [`objectdialog::render::open`] and cleared when its kind pops. The stage
    /// machine and provenance data contain no GPUI types; scrolling is separate.
    object_dialog: Option<objectdialog::ObjectDialogState>,
    /// Scroll state for the object dialog's row list — the
    /// `keybindings_scroll`/`settings_scroll` split, one dialog over.
    object_dialog_scroll: ScrollHandle,
    /// Pending config-write batch and rollback documents. This state outlives
    /// the dialog because the user can close it within the 250 ms debounce
    /// window; the write and any failure handling must still complete.
    pending_config_write: Option<objectdialog::apply::PendingConfigWrite>,
    /// Which scheduled config-write flush is the current one. Bumped by
    /// every applied edit; a flush task that wakes holding an older value
    /// has been superseded and does nothing, which is how N keystrokes
    /// coalesce into one write.
    config_write_seq: u64,
    /// Status notice for a failed config write or a disk write whose in-memory
    /// reload was rejected. Cleared by the next accepted flush. Kept on the
    /// shell because the debounced flush may finish after its dialog closes.
    pub(crate) config_write_error: Option<SharedString>,
    /// Today on the configured clock, refreshed by the reload poll. Rendering
    /// uses this cached date for the scope bar instead of reading the clock on
    /// every repaint. A date change triggers a new render.
    pub(super) today: chrono::NaiveDate,
}

/// Compare layered config documents by layer, name, path, and table content,
/// in order. Reload uses this to invalidate config-dependent state only
/// when its inputs actually change.
fn docs_equal(a: &[LayerDoc], b: &[LayerDoc]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.layer == y.layer && x.name == y.name && x.file == y.file && x.table == y.table
        })
}

impl ShellView {
    pub fn new(
        services: ShellServices,
        desk_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        // The toolbar's filter field: built once here, not per
        // render, so `Input`'s own cursor/selection/focus state survives
        // across frames. No placeholder — `toolbar::toolbar` names the
        // field with a search icon in the `Input`'s prefix slot instead,
        // the same way the palette and the dialogs' filter row do.
        let filter_input = cx.new(|cx| InputState::new(window, cx));

        // The scope bar's live text field:
        // one subscription for the life of the window, same lifecycle
        // shape as `palette_input`'s below. `Focus` opens a text-editing
        // session (`begin_scope_session`) and remembers the pre-focus
        // value for Escape to restore; `Change` feeds every keystroke
        // into the session, coalescing into one undo entry; `PressEnter`
        // and `Blur` both close the session (`end_scope_session`) — Enter
        // additionally hands focus home (`focus_home`: the open page, else
        // the shell root), `Blur` doesn't
        // need to (something else already has it). Escape's own restore
        // is handled in `handle_key_down`'s filter-focused branch, ahead
        // of this subscription ever seeing the resulting `Blur`.
        cx.subscribe_in(
            &filter_input,
            window,
            |view, input, event, window, cx| match event {
                InputEvent::Focus => {
                    view.filter_session_base = Some(input.read(cx).value().to_string());
                    view.active_frame()
                        .update(cx, |f, _| f.begin_scope_session());
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.active_frame().update(cx, |f, cx| {
                        let mut s = f.scope().clone();
                        s.text = (!text.trim().is_empty()).then_some(text);
                        if f.set_scope_in_session(s) {
                            cx.notify();
                        }
                    });
                }
                InputEvent::PressEnter { .. } => {
                    view.filter_session_base = None;
                    view.active_frame().update(cx, |f, _| f.end_scope_session());
                    view.focus_home(window, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.filter_session_base = None;
                    view.active_frame().update(cx, |f, _| f.end_scope_session());
                }
            },
        )
        .detach();

        // The palette input is stable for the life of the window and has no
        // placeholder; opening resets its value without replacing the entity.
        let palette_input = cx.new(|cx| InputState::new(window, cx));
        // A user edit updates the pure query state, resets selection, and
        // scrolls it into view. Programmatic `set_value` resets suppress Change
        // events, so opening the palette does not enter this path.
        cx.subscribe_in(&palette_input, window, |view, input, event, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let Some(palette) = view.palette.as_mut() else {
                return;
            };
            palette.set_query(input.read(cx).value().to_string());
            view.sync_palette_scroll();
            cx.notify();
        })
        .detach();

        // The per-tile command line's own input — same lifecycle
        // as `palette_input` above (built once, value reset on every
        // open, one `InputEvent::Change` subscription for the life of the
        // window). Unlike the palette's own subscription, this only ever
        // needs to re-rank completions — the actual key routing
        // (escape/enter/tab/ctrl+n/ctrl+p) happens in `handle_key_down`,
        // ahead of the window's own `Input` action bindings, exactly like
        // the modal branch above it.
        let command_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&command_input, window, |view, _input, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            view.on_command_line_changed(window, cx);
        })
        .detach();

        // The dialogs' shared filter field — same lifecycle as
        // `palette_input` above (see that field's doc comment), and the
        // same no-placeholder rule: `dialog::filter_row` puts a search
        // icon in the `Input`'s prefix slot instead.
        let dialog_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&dialog_input, window, |view, input, event, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let query = input.read(cx).value().to_string();
            // Route to the live dialog. While stacked, several of these fields are
            // `Some`; typing belongs to the top one only.
            match view.top_kind() {
                Some(dialog::DialogKind::Object) => {
                    if let Some(state) = view.object_dialog.as_mut() {
                        state.set_query(query);
                        // The top for a filter (the cursor just reset there); the
                        // edited row for an open plain field, which `set_query` keeps
                        // the cursor on — scrolling to 0 there would carry the list
                        // away from the row the trader is typing into.
                        // A choice field's rows are its ranked options, re-ranked
                        // by this keystroke: follow the lit one there instead.
                        let cursor = state
                            .draft
                            .as_ref()
                            .and_then(|d| d.choice_ranked_highlighted())
                            .unwrap_or_else(|| state.effective_selected());
                        view.object_dialog_scroll.scroll_to_item(cursor);
                    }
                }
                Some(dialog::DialogKind::Keybindings) => {
                    if let Some(state) = view.keybindings.as_mut() {
                        state.set_query(query);
                        view.keybindings_scroll.scroll_to_item(0);
                    }
                }
                Some(dialog::DialogKind::Settings) => {
                    if let Some(state) = view.settings.as_mut() {
                        state.set_query(query);
                        // Filtering resets the cursor to the top; a choice field
                        // re-ranks its options and the lit one is followed.
                        let row = state
                            .choice
                            .as_ref()
                            .map_or(0, |entry| entry.list.ranked_highlighted());
                        view.settings_scroll.scroll_to_item(row);
                    }
                }
                Some(dialog::DialogKind::Picker) => {
                    if let Some(state) = view.picker.as_mut() {
                        // No `set_query` method (unlike the two dialogs above) —
                        // `PickerState` has no other side effect to bundle with a
                        // query edit, so the two-line reset lives here rather than
                        // behind a one-line wrapper with a single caller.
                        state.query = query;
                        state.selected = 0;
                        picker::sync_picker_scroll(view);
                    }
                }
                Some(dialog::DialogKind::Choice) => {
                    if let Some(state) = view.choice_dialog.as_mut() {
                        // A choice list re-ranks on every keystroke and the lit
                        // row is followed, as the settings dialog's choice does.
                        state.set_query(&query);
                        view.choice_dialog_scroll
                            .scroll_to_item(state.list.ranked_highlighted());
                    }
                }
                Some(dialog::DialogKind::AsOf) => {
                    if let Some(state) = view.as_of_dialog.as_mut() {
                        // Re-ranking resets the highlight; keep the selected row visible.
                        asof_view::on_query_changed(state, &query);
                        view.as_of_scroll.scroll_to_item(asof_rows::child_index_of(
                            state.painted(),
                            state.highlighted(),
                        ));
                    }
                }
                Some(dialog::DialogKind::ScopeExpr) => {
                    if let Some(state) = view.scope_expr_dialog.as_mut() {
                        // The field IS the value; typing clears the last
                        // failed commit's message.
                        scope_expr_view::on_query_changed(state);
                    }
                }
                Some(dialog::DialogKind::Plain) | None => {}
            }
            // Typing changed the top dialog's query: re-rank its prepared rows.
            view.refresh_dialog_rows(cx);
            cx.notify();
        })
        .detach();
        // Expression suggestions follow the caret as well as the text. A
        // caret moved by an arrow or a click emits no `Change`, but the
        // input notifies, so observe it. `expr_suggest::refresh` compares
        // the text and caret before rebuilding; cursor blinks do not
        // repeat context analysis or ranking.
        cx.observe(&dialog_input, |view, _input, cx| {
            expr_suggest::refresh(view, cx)
        })
        .detach();

        // End tracking on window deactivation: the mouse release may land in
        // another app. Tile drags cancel without applying; divider drags keep
        // their live resizes and mark them dirty. Buttonless mouse moves also
        // terminate tracking if a platform does not report deactivation.
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active()
                && (view.tile_drag.is_some() || view.divider_drag.is_some())
            {
                view.cancel_tile_drag();
                view.cancel_divider_drag();
                cx.notify();
            }
        })
        .detach();

        // Seed the config snapshot on the background executor. Directory scans
        // perform filesystem I/O and must not run during UI construction.
        cx.spawn(async move |this, cx| {
            let mut is_first_poll = true;
            let mut memory = crate::memory::MemoryTracker::new();
            loop {
                cx.background_executor()
                    .timer(hot_reload::RELOAD_POLL_INTERVAL)
                    .await;

                // Sweep the flip barrier on the shared poll loop. An expired barrier
                // releases on the next iteration, so delay includes the timer, session
                // write, and config scan rather than only the timer interval.
                let Ok(frame) = this.update(cx, |view, _cx| view.frame.clone()) else {
                    return; // window/entity gone; stop polling
                };
                frame.update(cx, |f, cx| {
                    if f.sweep(Instant::now()) {
                        cx.notify();
                    }
                });

                // Refresh the diagnostics histogram only while a consumer watches it.
                // The source clone is gated as well; the destination compares before
                // copying and notifying.
                let Ok((diagnostics, watched)) = this.update(cx, |view, cx| {
                    let watched = view.diagnostics.read(cx).watchers() > 0;
                    (view.diagnostics.clone(), watched)
                }) else {
                    return; // window/entity gone; stop polling
                };
                if watched {
                    let Ok(perf) = this.update(cx, |view, _cx| view.perf.clone()) else {
                        return; // window/entity gone; stop polling
                    };
                    diagnostics.update(cx, |d, cx| {
                        if d.refresh_frame_hist(&perf) {
                            cx.notify();
                        }
                    });
                }

                // Sample process memory on every tick, watched or not, so the
                // `geode::memory` log keeps its peak record with the page
                // closed. The copy into diagnostics is gated on watchers and
                // on `refresh_memory`'s hysteresis, so idle jitter never
                // notifies.
                if let Some(sample) = crate::memory::sample() {
                    let log = memory.observe(sample, Instant::now(), SystemTime::now());
                    if let Some(reading) = memory.reading().copied() {
                        crate::memory::emit(log, &reading);
                        if watched {
                            diagnostics.update(cx, |d, cx| {
                                if d.refresh_memory(&reading) {
                                    cx.notify();
                                }
                            });
                        }
                    }
                }

                // Refresh the cached date and notify only when it changes. Rendering
                // reads this value without an additional clock read.
                let Ok(changed) = this.update(cx, |view, cx| {
                    let today = cx
                        .global::<crate::clock::AppClock>()
                        .0
                        .today(chrono::Utc::now());
                    let changed = view.today != today;
                    view.today = today;
                    changed
                }) else {
                    return; // window/entity gone; stop polling
                };
                if changed {
                    let _ = this.update(cx, |_view, cx| cx.notify());
                }

                // Extract session state on the UI thread and await its write on the
                // background executor. Run before config-scan early returns so saving
                // does not depend on a config change. Write failures are logged without
                // restoring the snapshot's dirty flag or comparison baselines.
                let Ok(pending_write) =
                    this.update(cx, |view, cx| view.take_dirty_session_write(cx))
                else {
                    return; // window/entity gone; stop polling
                };
                if let Some((path, text)) = pending_write {
                    cx.background_executor()
                        .spawn(async move { session::write_atomic(&path, &text) })
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(target: "geode::session", "failed to save session: {e}")
                        });
                }

                let Ok((desk_dir, user_dir)) = this.update(cx, |view, _cx| {
                    (view.desk_dir.clone(), view.user_dir.clone())
                }) else {
                    return; // window/entity gone; stop polling
                };

                let scan_desk = desk_dir.clone();
                let scan_user = user_dir.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { reload::scan(scan_desk.as_deref(), scan_user.as_deref()) })
                    .await;

                if is_first_poll {
                    is_first_poll = false;
                    // Seed-only: store this first background-thread scan as
                    // the baseline and move on, without comparing it to the
                    // `Snapshot::default()` placeholder — that comparison
                    // would always read as "changed" (default is empty,
                    // and a real desk/user dir practically never is) and
                    // trigger a reload of the config `new` was just handed,
                    // on every window open.
                    if this
                        .update(cx, |view, _cx| view.last_snapshot = snapshot)
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }

                let Ok(changed) = this.update(cx, |view, _cx| {
                    let changed = snapshot.changed_since(&view.last_snapshot);
                    if changed {
                        view.last_snapshot = snapshot.clone();
                    }
                    changed
                }) else {
                    return;
                };
                if !changed {
                    continue;
                }

                // The builtin layer is read off the live services rather
                // than rebuilt here (see `ShellServices::builtin`), and
                // only once a change has actually been seen — a clone per
                // 500ms poll would be pure per-frame churn for a reload
                // that almost never happens.
                let Ok(builtin) = this.update(cx, |view, _cx| view.services.builtin.clone()) else {
                    return;
                };
                let new_config = cx
                    .background_executor()
                    .spawn(async move { reload::load_config(builtin, desk_dir, user_dir) })
                    .await;

                if this
                    .update(cx, |view, cx| view.apply_reload(new_config, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();

        let font_size = FontSize::from_config(&services.config);
        let find_style = FindStyle::from_config(&services.config);
        let add_direction = crate::tileadd::AddDirection::from_config(&services.config);
        let line_numbers = crate::linenumbers::LineNumbers::from_config(&services.config);
        cx.set_global(crate::linenumbers::UiSettings { line_numbers });

        // `[timeseries] default_source` plus the fetch sources it names,
        // one of the workspace's five globals (see `series`'s module doc). Set
        // here and re-derived on reload; the settings row writes both
        // through `set_default_source`.
        let series = crate::series::SeriesSettings::from_config(&services.config);
        let default_source = series.default_source.clone();
        cx.set_global(series);
        // The app-wide clock (`crate::clock::AppClock`, another of the workspace's
        // five globals — see its own doc comment).
        let (clock, clock_diags) = geode_core::clock::Clock::from_config(&services.config);
        cx.set_global(crate::clock::AppClock(clock));
        // Empty until the bridge's first live reference answer: a module
        // reading it before then sees no table rather than a missing global.
        cx.set_global(crate::reference::ReferenceGlobal::default());

        // Publish bindings for module tooltip chord lookup through `tips::Chords`.
        cx.set_global(crate::tips::Chords(Arc::new(
            services.keymap.bindings().to_vec(),
        )));

        // The shared frame: built from whatever `[groupings]`/
        // `[scopes]` (plus the `datasets`/`dimensions` docs they validate
        // against) config resolved to — see `hot_reload::rebuild_slots`/
        // `rebuild_saved_scopes`, shared with `apply_reload`'s own
        // rebuilds.
        let frame = {
            let slots = hot_reload::rebuild_slots(&services.config);
            // Report saved-scope diagnostics here. Startup action registration
            // loads the same scopes with reporting disabled to avoid duplicate logs.
            let saved = hot_reload::rebuild_saved_scopes(&services.config, true);
            let named = hot_reload::rebuild_named_expressions(&services.config);
            cx.new(|_| {
                let mut frame = Frame::new(slots, saved, user_dir.clone());
                frame.replace_named_expressions(named);
                frame
            })
        };
        // Observe the frame before creating occupants. GPUI notifies observers
        // in registration order, so `on_frame_changed` opens the flip barrier
        // before any occupant reacts or reports its arrival.
        //
        // This observer also drains pending config writes and reflects scope
        // text into the input. It needs `observe_in` because input updates
        // require a `Window`.
        cx.observe_in(&frame, window, |view, frame, window, cx| {
            view.on_frame_changed(frame, window, cx)
        })
        .detach();

        // The shell-owned diagnostics gatherer, created
        // alongside the frame — see the field's own doc comment. Seeded
        // from `services.log`'s levels when logging is wired up (`None`
        // in every test setup that doesn't opt in, mirroring `log`
        // itself), `LogLevels::default()` otherwise.
        let diagnostics = {
            let levels = services
                .log
                .as_ref()
                .map(|l| l.levels.clone())
                .unwrap_or_default();
            cx.new(|_| Diagnostics::new(levels))
        };
        // Seed diagnostics from config loading, derived settings validation,
        // and startup keymap compilation. Keep this order aligned with
        // `apply_reload` so the section is consistent before and after reload.
        let startup_diagnostics = {
            let cfg = &services.config;
            let mut diags = cfg.diagnostics.clone();
            diags.extend(crate::defaults::mod_alias_from_config(cfg).1);
            diags.extend(crate::defaults::modules_default_diagnostic(cfg));
            // Warn when the default timeseries source names no fetch source.
            diags.extend(crate::series::default_source_diagnostic(cfg));
            diags.extend(clock_diags.iter().cloned());
            diags.extend(services.keymap_diagnostics.iter().cloned());
            diags.extend(services.composition_diagnostics.iter().cloned());
            diags
        };
        diagnostics.update(cx, |d, _cx| {
            d.note_config(startup_diagnostics, std::time::SystemTime::now());
        });
        // Window-bound like the frame's observer above: the page-open drain
        // moves focus to the page.
        cx.observe_in(&diagnostics, window, |view, diagnostics, window, cx| {
            view.on_diagnostics_changed(diagnostics, window, cx);
        })
        .detach();

        // Restore palette usage and frame state. Clearing scope history removes
        // the undo entry created by `set_scope`, so startup does not offer an
        // undo back to the initial empty scope.
        let palette_usage = services.restored_palette_usage.clone();
        if let Some(record) = services.restored_frame.clone() {
            // `[frame]` is the shared lane's record; pinned lanes restore
            // from their own workspace records.
            frame.update(cx, |f, _cx| {
                let mut s = f.shared_mut();
                s.set_scope(record.scope);
                s.set_active_slot(record.active_slot);
                if let Some(chain) = record.ad_hoc {
                    s.restore_ad_hoc(chain, record.ad_hoc_active);
                }
                s.set_as_of(record.as_of);
                s.clear_history();
            });
        }
        // Each restored pinned workspace gets its own lane. Clearing history
        // keeps startup from offering an undo back to the empty scope; this
        // runs before the flip seed below so a restored lane never reads as
        // "just changed".
        for (ws, record) in services.restored_pinned.clone() {
            // A pin on a workspace the layout lacks would surface as an
            // unexpected pin if that workspace were created later. Both come
            // from one session read, so this only guards a hand-assembled
            // `ShellServices`.
            if !services.workspaces.spaces().any(|(ix, _)| ix == ws.get()) {
                tracing::warn!(
                    target: "geode::session",
                    "pinned frame for workspace {} has no workspace; ignored",
                    ws.get()
                );
                continue;
            }
            frame.update(cx, |f, _| {
                f.pin(ws);
                let mut lane = f.view_mut(ws);
                lane.set_scope(record.scope);
                // Pinning copied the shared slot and ad hoc chain. Clear
                // both first so a recorded slot that is now empty (refused
                // below) leaves no slot rather than the shared lane's, and a
                // record without a chain does not keep the shared lane's
                // chain, which the session writer would then save as this
                // workspace's own.
                lane.set_active_slot(None);
                lane.forget_ad_hoc();
                lane.set_active_slot(record.active_slot);
                // After the slot, so an active chain wins over it; before
                // `clear_history`, like every other restored value.
                if let Some(chain) = record.ad_hoc {
                    lane.restore_ad_hoc(chain, record.ad_hoc_active);
                }
                lane.set_as_of(record.as_of);
                lane.clear_history();
            });
        }
        // A session written under another configuration can name columns
        // this one cannot group by. Checked once for every restored lane.
        let groupable = hot_reload::groupable_names(&services.config);
        frame.update(cx, |f, _| {
            for column in f.retain_ad_hoc(|column| groupable.iter().any(|g| g == column)) {
                tracing::warn!(
                    target: "geode::session",
                    "restored ad hoc grouping dropped: '{column}' is not a groupable column"
                );
            }
        });
        // A group keeps its last scope across a restart. Restored before any
        // membership is applied and before the flip baselines below are
        // seeded, and nothing is notified: a restored follower's first
        // query is already scoped by its group, with no emitter needed to
        // post it again, and the restored scope is not read as a change.
        // Lost, a follower that showed one underlying would show the whole
        // book under the same chip.
        if services
            .restored_links
            .iter()
            .any(|scope| !scope.is_empty())
        {
            frame.update(cx, |f, _| {
                for group in geode_core::link::Group::ALL {
                    let scope = &services.restored_links[group.index()];
                    if !scope.is_empty() {
                        f.restore_group_scope(group, scope.clone());
                    }
                }
            });
        }
        // A restored tile is in its link groups before its occupant exists,
        // so the first query it sends is already scoped by the group it
        // follows. Two conditions, each for a tile that will never have a
        // module occupant to end the membership. The record's kind has a
        // factory: without one the tile paints a placeholder, which is in
        // no group, and the record carries the membership to the next save.
        // The tile is placed in some workspace, on screen or not: a record
        // the layout's healing left behind gets no occupant at all.
        // Nothing is notified. A membership moves no lane version and no
        // group's scope generation, so it is not a flip either.
        if services
            .restored_tiles
            .values()
            .any(|record| !record.link.is_empty())
        {
            frame.update(cx, |f, _| {
                for (id, record) in &services.restored_tiles {
                    let tile = TileId(*id);
                    if services.roster.factory(&record.kind).is_some()
                        && services.workspaces.workspace_of(tile).is_some()
                    {
                        f.follow(tile, record.link.follow);
                        f.emit(tile, record.link.emit);
                    }
                }
            });
        }
        // Seeded from the just-built frame (see the field's own doc
        // comment) so a restored session's scope/slot/as-of is never
        // itself read as "just changed" by the first real
        // `on_frame_changed`.
        let last_flip_versions = frame
            .read(cx)
            .view(services.workspaces.active_ix())
            .versions();
        let last_flip_groups = frame.read(cx).group_scope_gens();

        // The docs the data engine actually starts with — see
        // `sources_baseline`'s field doc.
        let sources_baseline = services.config.layered_docs("sources").to_vec();
        let datasets_baseline = services.config.layered_docs("datasets").to_vec();
        let egress_baseline = services.config.layered_docs("egress").to_vec();
        let positions_baseline = services.config.layered_docs("positions").to_vec();
        let panels_baseline = services
            .config
            .layered_docs(geode_core::panel::PANELS_DOC)
            .to_vec();
        // Same reasoning, for the `[pricing] adapter` key the data
        // engine's pricer was chosen from — see
        // `pricing_baseline`'s field doc.
        let pricing_baseline = services.config.get("app", "pricing.adapter").cloned();
        // And for the `[vol] model` key — see `vol_baseline`'s field doc.
        let vol_baseline = services.config.get("app", "vol.model").cloned();
        // The dimension pickers' column list — see
        // `pickable`'s field doc.
        let pickable = pickable_columns(&services.config);
        let expr_vocab = std::rc::Rc::new(expr_vocab(&services.config));
        let page_entries: Vec<crate::module::PageEntry> = services.pages.entries().collect();

        Self {
            services,
            matcher: Matcher::default(),
            font_size,
            find_style,
            focus_handle,
            palette: None,
            palette_usage,
            palette_usage_version: 0,
            last_palette_usage_written: 0,
            modals: Vec::new(),
            keybindings: None,
            keybindings_scroll: ScrollHandle::new(),
            settings: None,
            settings_scroll: ScrollHandle::new(),
            palette_scroll: UniformListScrollHandle::new(),
            palette_input,
            command_input,
            command_line: None,
            command_scroll: ScrollHandle::new(),
            fuzzy_find: None,
            fuzzy_find_subscriptions: Vec::new(),
            dialog_input,
            desk_dir,
            user_dir,
            last_snapshot: reload::Snapshot::default(),
            last_reload: reload::ReloadOutcome::Unchanged,
            reload_status: None,
            config_revision: 0,
            session_dirty: false,
            last_tiles_written: crate::session::TileRecords::new(),
            last_pages_written: crate::session::PageRecords::new(),
            last_frame_generation_written: 0,
            last_session_text: None,
            pending_focus_restore: false,
            swallow_double_click_followup: false,
            overlay_return_to_filter: false,
            divider_drag: None,
            tile_drag: None,
            filter_input,
            filter_session_base: None,
            perf: FrameHistogram::new(),
            last_render_started: None,
            perf_overlay: false,
            page: None,
            page_entries,
            frame,
            diagnostics,
            pending_tiles: BTreeMap::new(),
            unplaced_records: crate::session::TileRecords::new(),
            add_direction,
            line_numbers,
            default_source,
            last_flip_versions,
            last_flip_groups,
            occupants: HashMap::new(),
            visible_tiles: HashSet::new(),
            emit_subs: HashMap::new(),
            link_label: None,
            stack_sent: HashMap::new(),
            focused_sent: None,
            notice: None,
            stack_list: None,
            add_filter_menu: None,
            row_menu: None,
            scratch_all_tiles: HashSet::new(),
            scratch_active_tiles: HashSet::new(),
            scratch_visible_keys: Vec::new(),
            restart_required: None,
            sources_baseline,
            datasets_baseline,
            egress_baseline,
            positions_baseline,
            panels_baseline,
            pricing_baseline,
            vol_baseline,
            pickable,
            expr_vocab,
            picker: None,
            next_picker_tag: 0,
            picker_scroll: UniformListScrollHandle::new(),
            as_of_dialog: None,
            as_of_scroll: ScrollHandle::new(),
            as_of_data_version: 0,
            scope_expr_dialog: None,
            choice_dialog: None,
            choice_dialog_scroll: ScrollHandle::new(),
            expr_scroll: ScrollHandle::new(),
            object_dialog: None,
            object_dialog_scroll: ScrollHandle::new(),
            pending_config_write: None,
            config_write_seq: 0,
            config_write_error: None,
            today: clock.today(chrono::Utc::now()),
        }
    }

    /// Close the live (topmost) modal: clear only its kind's state, then give the
    /// revealed dialog back its input and focus, or, when none remains, return
    /// focus to where the first dialog was opened from. Escape, the close button,
    /// and backdrop clicks use this same path.
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(top) = self.modals.pop() {
            self.clear_dialog_state(top.kind);
        }
        // A revealed dialog may have missed reloads while it was covered.
        self.refresh_dialog_rows(cx);
        if self.modals.is_empty() {
            // A same-kind refusal's notice names a kind lower in the stack; once
            // the stack is empty that kind no longer exists, so the notice must
            // not linger describing a dialog nothing points to.
            if self
                .notice
                .as_deref()
                .is_some_and(dialog::is_already_open_notice)
            {
                self.notice = None;
            }
            self.return_focus_from_overlay(window, cx);
        } else {
            dialog::refocus_top(self, window, cx);
        }
        cx.notify();
    }

    /// Drop the state field `kind` owns, and for an object dialog bring back the
    /// one it covered. A field left behind would swallow the next same-kind
    /// dialog's queries.
    fn clear_dialog_state(&mut self, kind: dialog::DialogKind) {
        use dialog::DialogKind;
        match kind {
            DialogKind::Settings => self.settings = None,
            DialogKind::Keybindings => self.keybindings = None,
            DialogKind::Picker => self.picker = None,
            DialogKind::AsOf => self.as_of_dialog = None,
            DialogKind::ScopeExpr => self.scope_expr_dialog = None,
            DialogKind::Choice => self.choice_dialog = None,
            DialogKind::Object => {
                self.object_dialog = None;
                // The next object dialog down, if any, becomes live again.
                dialog::unpark_object_dialog(self);
            }
            DialogKind::Plain => {}
        }
    }

    /// Whether any modal is open.
    pub(crate) fn modal_open(&self) -> bool {
        !self.modals.is_empty()
    }

    /// The live (topmost) modal.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn top_modal(&self) -> Option<&dialog::ShellModal> {
        self.modals.last()
    }

    /// The live modal's kind, which decides who owns the shared input and keys.
    pub(crate) fn top_kind(&self) -> Option<dialog::DialogKind> {
        self.modals.last().map(|m| m.kind)
    }

    /// How many modals are stacked.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn modal_depth(&self) -> usize {
        self.modals.len()
    }

    /// Where focus goes when an overlay closes: back to the scope bar's
    /// text field if it was focused when the overlay opened
    /// (`overlay_return_to_filter`, consumed here), home (`focus_home`)
    /// otherwise. The one door both `close_modal` and `close_palette` use.
    /// The field is painted over a page too, so either return is live there.
    pub(super) fn return_focus_from_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::mem::take(&mut self.overlay_return_to_filter) {
            self.filter_input
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            // The open page's handle when one is open, else the shell root.
            self.focus_home(window, cx);
        }
    }

    /// Does the scope bar's text field hold keyboard focus right now?
    pub(super) fn filter_field_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    /// Respond to frame notifications: coordinate visible-tile flips, refresh
    /// as-of rows, drain pending scope/grouping writes in the background, and
    /// reflect scope text into the input. The frame itself performs no I/O.
    fn on_frame_changed(
        &mut self,
        frame: Entity<Frame>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Open a flip barrier for scope, grouping, or as-of changes so visible
        // tiles publish the new frame together. Data/config changes query
        // independently and do not open a barrier. This observer is registered
        // before occupants, ensuring the full key set is ready before their
        // frame callbacks run, including non-following tiles that self-arrive.
        // Visible tiles are the active workspace's, so the flip compares and
        // opens against the active lane, not a dialog's target lane. A link
        // group's scope is part of what its followers show, so a change to
        // it flips them as well.
        let ws = self.active_ix();
        let (lane_now, groups_now) = {
            let f = frame.read(cx);
            (f.view(ws).versions(), f.group_scope_gens())
        };
        let (last_lane, last_groups) = (self.last_flip_versions, self.last_flip_groups);
        let lane_moved = !lane_now.same_flip_identity(last_lane);
        let groups_moved = groups_now != last_groups;
        if lane_moved || groups_moved {
            self.last_flip_versions = lane_now;
            self.last_flip_groups = groups_now;
            let mut keys = std::mem::take(&mut self.scratch_visible_keys);
            self.visible_tile_keys(&mut keys);
            frame.update(cx, |f, _| {
                // Each visible tile answers under its own identity: a
                // follower's scope generation is its group's, and enrolled
                // under the lane's it could never arrive. A lane change
                // awaits every visible tile; a group's scope change awaits
                // only that group's followers.
                let awaited: Vec<(QueryKey, FrameVersions)> = keys
                    .iter()
                    .filter_map(|key| {
                        // A tile's query key is its tile id, the convention
                        // every module follows.
                        let tile = TileId(key.0);
                        let follows = f.membership(tile).follow;
                        let concerned = lane_moved
                            || follows
                                .is_some_and(|g| groups_now[g.index()] != last_groups[g.index()]);
                        concerned.then(|| (*key, f.view_for(ws, tile).versions()))
                    })
                    .collect();
                if lane_moved {
                    // A new flip for everyone: whatever was awaited before
                    // answered an older frame.
                    f.open_flip_each(awaited, Instant::now());
                } else {
                    // A group's change alone joins the flip in progress.
                    // Replacing it would drop the other tiles while their
                    // queries are in flight: they would paint on arrival,
                    // beside tiles still holding what they staged. A group
                    // with no visible follower adds nothing, and so leaves
                    // an open barrier waiting.
                    f.extend_flip(awaited, Instant::now());
                }
            });
            self.scratch_visible_keys = keys;
            // The shared reload poll sweeps the deadline; no timer is needed
            // for each frame mutation.
        }
        // Refresh an open as-of dialog when publishes change. Scope, grouping,
        // and as-of edits do not rebuild rows while the user is filtering.
        if self.as_of_dialog.is_some() && lane_now.data != self.as_of_data_version {
            self.as_of_data_version = lane_now.data;
            let as_of = self.target_frame().read(cx).as_of().clone();
            let publishes: Vec<_> = frame.read(cx).recent_publishes().iter().cloned().collect();
            if let Some(state) = self.as_of_dialog.as_mut() {
                state.refresh(&as_of, &publishes, chrono::Utc::now());
                // Keep the identity-preserved highlight visible after refreshing rows.
                self.as_of_scroll.scroll_to_item(asof_rows::child_index_of(
                    state.painted(),
                    state.highlighted(),
                ));
            }
        }
        if let Some((slot, grouping)) = frame.update(cx, |f, _| f.take_pending_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
                if let Err(e) = crate::frame::persist_slot_to_user_config(&dir, slot, &grouping) {
                    tracing::warn!(target: "geode::config", "{e}");
                }
            })
            .detach();
        }
        // Drain scope saves in the background, just like grouping saves. The
        // Scopes dialog writes through `config_write` directly; this handles
        // requests queued on the frame itself.
        if let Some((name, scope)) = frame.update(cx, |f, _| f.take_pending_scope_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
                if let Err(e) = crate::frame::persist_scope_to_user_config(&dir, &name, &scope) {
                    tracing::warn!(target: "geode::config", "{e}");
                }
            })
            .detach();
        }
        // A pressed header link chip opens the chooser on its own tile.
        if let Some(tile) = frame.update(cx, |f, _| f.take_pending_link_chooser()) {
            self.open_link_chooser_on(tile, window, cx);
        }
        // Reflect external scope changes into an unfocused input. While it is
        // focused, its text remains authoritative; replacing the value would
        // interrupt the user's caret and selection.
        if !self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            let frame_text = self
                .active_frame()
                .read(cx)
                .scope()
                .text
                .clone()
                .unwrap_or_default();
            let field_text = self.filter_input.read(cx).value().to_string();
            if field_text != frame_text {
                self.filter_input.update(cx, |i, cx| {
                    i.set_value(frame_text, window, cx);
                });
            }
        }
        // A dialog whose list reads the frame (the Groupings list's leading
        // rows) re-derives when the frame changes under it; a stale `*` row
        // would offer a chain that no longer exists. Last, after every
        // `frame.update` above, so the key carries the final generation.
        if self
            .object_dialog
            .as_ref()
            .is_some_and(|state| state.domain.applies_from_browse())
        {
            self.refresh_dialog_rows(cx);
        }
        cx.notify();
    }

    /// Apply queued log-level, overlay and page-open requests from shared
    /// diagnostics. Modules access that entity without reaching `ShellView`.
    /// Catalog requests are drained by the app bridge, which owns
    /// data-service access.
    fn on_diagnostics_changed(
        &mut self,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pending_level, pending_overlay, pending_page) = diagnostics.update(cx, |d, _cx| {
            (
                d.take_pending_level(),
                d.take_pending_overlay_toggle(),
                d.take_pending_diagnostics_page(),
            )
        });
        if let Some((target, level)) = pending_level {
            let levels = diagnostics.read(cx).levels.clone();
            if let Some(log) = &self.services.log
                && let Err(e) = log.control.set(&levels)
            {
                tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
            }
            if let Some(dir) = self.user_dir.clone() {
                crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
                    if let Err(e) =
                        log_persist::persist_log_level_to_user_config(&dir, &target, level)
                    {
                        tracing::warn!(target: "geode::config", "failed to persist [log]: {e}");
                    }
                })
                .detach();
            }
        }
        if pending_overlay {
            let next = !self.perf_overlay;
            self.set_perf_overlay(next, cx);
        }
        if let Some(source) = pending_page {
            self.open_page_on_request(
                crate::diagnostics::DIAGNOSTICS_PAGE_KIND,
                &source,
                window,
                cx,
            );
        }
        cx.notify();
    }

    /// Set the overlay and mirror it into `Diagnostics` in one place. Both
    /// the keyboard action and the entity's toggle channel come through here
    /// so the page's switch and the readout can never disagree.
    pub(super) fn set_perf_overlay(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.perf_overlay = visible;
        self.diagnostics.update(cx, |d, cx| {
            if d.set_overlay_visible(visible) {
                cx.notify();
            }
        });
    }

    /// Set a grouping slot in memory and notify the frame observer, which
    /// writes it to the user layer in the background. The Groupings dialog
    /// uses its own config-write path; this entry point is exercised by
    /// locality tests.
    pub fn save_slot(
        &mut self,
        slot: u8,
        grouping: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.frame.update(cx, |f, cx| {
            let result = f.save_slot(slot, grouping);
            if result.is_ok() {
                cx.notify();
            }
            result
        })
    }

    /// The current config, for the app bridge to read the new `views` doc
    /// out of after a `ShellEvent::ConfigReloaded` — `geode-app` is
    /// the only crate allowed to touch `geode-data`, so it needs to reach
    /// the reloaded config through the shell rather than reloading it a
    /// second time itself.
    pub fn config(&self) -> &Config {
        &self.services.config
    }

    /// The shared frame entity every occupant holds. Lane state (scope,
    /// grouping, as-of) is reached through `target_frame` or
    /// [`Self::active_frame`], which name the workspace.
    pub fn frame(&self) -> &Entity<Frame> {
        &self.frame
    }

    pub(crate) fn active_ix(&self) -> WorkspaceIx {
        self.services.workspaces.active_ix()
    }

    pub(crate) fn frame_at(&self, ws: WorkspaceIx) -> FrameRef {
        FrameRef::new(self.frame.clone(), ws)
    }

    /// The lane every shell surface reads and writes: the workspace the
    /// open modal stack was opened from, else the active one. A dialog
    /// therefore commits where it was opened, whatever is active later.
    pub(crate) fn target_frame(&self) -> FrameRef {
        let ws = self
            .modals
            .first()
            .map(|m| m.workspace)
            .unwrap_or_else(|| self.active_ix());
        self.frame_at(ws)
    }

    /// Load saved scope `name` into [`Self::target_frame`]'s lane through
    /// `load_scope` (so it is one undoable `set_scope` step and honours a
    /// workspace pin), notifying on a change. The one path both the
    /// `scope::<name>` actions and the scope picker take. `Err` when no
    /// saved scope has that name; `Ok(false)` when it is already current.
    pub(crate) fn load_saved_scope(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        self.target_frame().update(cx, |f, cx| {
            let loaded = f.load_scope(name);
            if let Ok(true) = loaded {
                cx.notify();
            }
            loaded
        })
    }

    /// The active workspace's frame, for the app's catalog as-of.
    pub fn active_frame(&self) -> FrameRef {
        self.frame_at(self.active_ix())
    }

    /// The shared diagnostics entity available to occupants.
    pub fn diagnostics(&self) -> &Entity<Diagnostics> {
        &self.diagnostics
    }

    /// The open dimension picker's state, if any —
    /// cross-crate test reach only, the same door `module::recording`
    /// opens for `geode-blotter`'s tests: `geode-app`'s bridge tests need
    /// to see a picker's `values` land (or fail to) without a `dispatch`
    /// call of their own to drive from (`dispatch` is `pub(super)`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn picker(&self) -> Option<&picker::PickerState> {
        self.picker.as_ref()
    }

    /// Apply a loaded candidate exactly as the file watcher would —
    /// cross-crate test reach, the same door `picker()` opens: `geode-app`'s
    /// composition tests reload without a watcher (`apply_reload` is
    /// `pub(super)`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn apply_reload_for_test(&mut self, config: Config, cx: &mut Context<Self>) {
        self.apply_reload(config, cx);
    }

    /// The running shell's services (registry, roster) — cross-crate test
    /// reach, the same door as `picker()`: `geode-app`'s composition tests
    /// check that a reload added no tile kind and no add-tile action.
    #[cfg(any(test, feature = "test-support"))]
    pub fn services(&self) -> &ShellServices {
        &self.services
    }

    /// The title `tile`'s occupant paints, or `None` without an occupant —
    /// cross-crate test reach, the same door as `picker()`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn occupant_title(&self, tile: TileId, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.occupants.get(&tile).map(|o| o.content.title(cx))
    }

    /// The open page's kind, or `None` while no page is open — cross-crate
    /// test reach, the same door as `picker()`: `geode-app`'s tests open the
    /// page from a tile's health chip, a route with no shell dispatch.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_page_kind_for_test(&self) -> Option<&'static str> {
        self.open_page_kind()
    }

    /// The open page's own focus handle — cross-crate test reach, the same
    /// door as `picker()`: `geode-app`'s tests check that a key the page
    /// handled left keyboard focus on the page.
    #[cfg(any(test, feature = "test-support"))]
    pub fn page_focus_handle_for_test(&self, cx: &gpui::App) -> Option<gpui::FocusHandle> {
        self.page
            .as_ref()
            .filter(|p| p.open)
            .map(|p| p.occupant.content.focus_handle(cx))
    }

    /// The status notice's text (painted under `shell-notice`), or `None`
    /// without one — cross-crate test reach, the same door as `picker()`:
    /// `geode-app`'s tests read what a row menu action reported.
    #[cfg(any(test, feature = "test-support"))]
    pub fn notice_for_test(&self) -> Option<SharedString> {
        self.notice.clone()
    }

    /// The configured clock (`AppClock`), for the shell's own painters.
    pub fn clock(&self, cx: &gpui::App) -> geode_core::clock::Clock {
        cx.global::<crate::clock::AppClock>().0
    }

    /// The dialogs' shared filter field — cross-module test reach the
    /// same as `picker()` above.
    #[cfg(any(test, feature = "test-support"))]
    pub fn dialog_input(&self) -> &Entity<InputState> {
        &self.dialog_input
    }

    /// The open choice dialog's target, for module-hosting tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn choice_dialog_target(&self) -> Option<choicedialog::Target> {
        self.choice_dialog.as_ref().map(|d| d.target.clone())
    }

    /// Deliver a distinct-value reply from the app bridge. `EXPR_KEY` routes
    /// to the open expression field's suggestions. `SCOPES_KEY` routes
    /// to the object dialog's Values stage. `ACTION_KEY` routes to an open
    /// action value choice. Other replies reach the dimension
    /// picker only if it is open in Values stage and both column and latest
    /// request tag match. Stale replies cause no mutation or notification.
    pub fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        if outcome.key == EXPR_KEY {
            expr_suggest::deliver(self, outcome, cx);
            return;
        }
        if outcome.key == SCOPES_KEY {
            objectdialog::deliver_values(self, outcome, cx);
            return;
        }
        if outcome.key == ACTION_KEY {
            choicedialog::deliver_action_values(self, outcome, cx);
            return;
        }
        let Some(state) = self.picker.as_mut() else {
            return;
        };
        let picker::Stage::Values { column } = &state.stage else {
            return;
        };
        if *column != outcome.column || outcome.tag != state.tag {
            return;
        }
        state.values = Some(outcome.values);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
