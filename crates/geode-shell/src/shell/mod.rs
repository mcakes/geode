//! The shell's window root: owns key dispatch, workspace state, overlays,
//! and tile occupants. The toolbar serves as the native title bar; the
//! sidebar and status bar surround the remaining tile area. Rendering,
//! input routing, persistence, and occupant lifecycle live in sibling modules.

mod add_tile;
pub mod aggregates;
pub mod app;
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
mod scopedialog;
mod session_io;
pub mod settings_view;
pub mod sidebar;
pub mod stacklist;
pub mod status;
pub mod toolbar;
mod value_color;
pub mod whichkey;

pub use crate::tiling::WindowKey;
pub use app::{OpenRefusal, Placement, Route, ShellApp, WindowOpener, WindowSpec};
pub use keys::convert_keystroke;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    App, Context, Entity, FocusHandle, Focusable as _, ScrollHandle, SharedString,
    UniformListScrollHandle, Window,
};
use gpui_component::input::{InputEvent, InputState};

use crate::actions::ActionRegistry;
use crate::commandline::CommandLine;
use crate::diagnostics::{ActionTail, Diagnostics};
use crate::frame::{Frame, FrameRef, FrameVersions};
use crate::keymap::{Keymap, Matcher, Modifiers};
use crate::log_persist;
use crate::module::{ModuleRoster, PageRoster, TileOccupant};
use crate::palette::PaletteState;
use crate::perf::FrameHistogram;
use crate::theme::ThemeService;
use crate::tiling::{TileId, Workspace, WorkspaceIx, Workspaces};
use geode_core::config::{Config, ConfigSources, Diagnostic, LayerDoc};
use geode_core::dimensions::DerivedDimensions;
use geode_core::log::{LevelControl, LogLevels, Ring};
use geode_core::query::{DistinctOutcome, QueryKey};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::sync::{Arc, Mutex};

/// Everything the shell needs to run a window, assembled once by the app
/// from loaded config, the action registry, the compiled keymap, and the
/// initial workspace state. [`ShellApp`] owns this for the life of the
/// process; every window's `ShellView` reads it through its app entity.
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
    /// Module names and state from `session.toml`. `ShellApp::new` parks
    /// them; a window drains the ones it hosts at construction
    /// (`ShellApp::take_parked_records`) and consumes each as it creates
    /// the occupant. Unavailable kinds retain their records separately.
    pub restored_tiles: crate::session::TileRecords,
    /// Optional scope, grouping slot, and as-of from `[frame]`.
    /// `ShellApp::new` applies them and clears scope undo/redo history.
    pub restored_frame: Option<crate::session::FrameRecord>,
    /// Pinned workspace lanes from `session.toml`'s `workspaces.N.frame`;
    /// `ShellApp::new` pins each workspace and fills its lane with clean
    /// scope history.
    pub restored_pinned: crate::session::PinnedRecords,
    /// Link group scopes from `session.toml`'s `[links.<letter>]`, in
    /// `Group::ALL` order; `ShellApp::new` sets each non-empty one on its
    /// group before any tile is linked or created.
    pub restored_links: crate::session::GroupScopes,
    /// The palette's usage history from `session.toml`'s `[palette.usage]`
    /// table — empty for a fresh session and in every test setup that
    /// doesn't opt in. `ShellApp::new` takes it as the live history.
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
    /// The last run's window records from `session.toml`'s `[windows.K]`,
    /// in K order; empty for a fresh session and in every test setup that
    /// doesn't opt in. `ShellApp::new` takes them for the startup that
    /// opens a window per record (`ShellApp::take_restored_windows`).
    pub restored_windows: Vec<crate::session::WindowRecord>,
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

/// Events [`ShellApp`] emits: configuration changes and distinct-value
/// requests for the app bridge, and what every window reacts to in its
/// `on_app_event` (`Reloaded`, `DayChanged`, `Park`). The bridge submits
/// data requests and delivers their outcomes; the shell does not depend on
/// `geode-data`.
///
/// `PartialEq` follows `DistinctParams`: its scope can contain floating-point
/// literals and therefore does not implement `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellEvent {
    /// An accepted reload was applied by the app: `revision` is the new
    /// config revision, and each flag names a per-window rebuild the
    /// reload needs (the dimension pickers' columns, the palette's
    /// snapshot, the link groups' composition rule, the named
    /// expressions an open expression dialog offers). Queued after
    /// `ConfigReloaded` and `AppSettingsReloaded`, before the frame's
    /// notifications, so a window sees the revision only after the
    /// bridge refreshed its views.
    Reloaded {
        revision: u64,
        pickable_changed: bool,
        palette_snapshot_changed: bool,
        link_filter_changed: bool,
        named_changed: bool,
    },
    /// The configured clock's date moved since the watcher's last tick;
    /// every window repaints its scope bar from `ShellApp::today`.
    DayChanged,
    /// `workspace` has left window `window` for another (a switch that
    /// `Moved` it, or a new window opening on it): `window` parks the
    /// occupants it built for it (`ShellView::park_workspace`), then
    /// answers with `ShellApp::records_parked`. Emitted by `ShellApp::switch`
    /// and `ShellApp::add_window` once `Workspaces` has committed the move,
    /// never for a raise, a refusal or a switch within one window.
    Park {
        window: WindowKey,
        workspace: WorkspaceIx,
    },
    /// The window that hosted `workspace` has parked its occupants' records
    /// (`ShellApp::parked_tiles`): the window now showing it pulls them and
    /// rebuilds on its next draw. A second event rather than part of
    /// `Park`, because subscriber order is not a contract: one event could
    /// not order the source's park before the destination's pull.
    Parked { workspace: WorkspaceIx },
    /// A window joined or left the registry (`ShellApp::open_window`
    /// confirmed one, `ShellApp::window_closed` forgot one): every window
    /// re-reads `ShellApp::window_title`, which names the shown index
    /// only with two or more windows. The title is set from this event,
    /// never from render.
    WindowsChanged,
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

/// The scope expression suggestions' distinct-values requests (the frame
/// expression dialog and the Scope dialog's definition step). Routed by
/// [`ShellView::deliver_distinct`] to `expr_suggest::deliver`, which drops
/// any reply whose tag is not its column's latest.
pub const EXPR_KEY: QueryKey = QueryKey(u64::MAX - 4);

/// A row action's value choice (`ActionCx::choose_value`) submits its
/// `Request::Distinct` under this key — one lower than `EXPR_KEY`, same
/// reservation reasoning. [`ShellView::deliver_distinct`] routes it to the
/// open choice dialog, which drops a reply whose tag is not its own.
pub const ACTION_KEY: QueryKey = QueryKey(u64::MAX - 5);

/// Whether `key` is one of the shell's own reserved query keys. A distinct
/// outcome under any other key belongs to the tile with that id.
pub fn is_shell_key(key: QueryKey) -> bool {
    [PICKER_KEY, DIAGNOSTICS_KEY, EXPR_KEY, ACTION_KEY].contains(&key)
}

/// The bridge's live reference reads (`Request::Reference` at `AsOf::Live`)
/// submit under this key — one lower than `ACTION_KEY`, same reservation
/// reasoning. Their answers never reach the shell's delivery routes: the
/// bridge keeps them in [`crate::reference::ReferenceGlobal`], dropping any
/// whose tag is not its dataset's latest.
pub const REFERENCE_KEY: QueryKey = QueryKey(u64::MAX - 6);

/// The bridge's watchlist resolutions (`WatchlistGlobal`) submit under one
/// key PER LIST from this range, `WATCHLIST_KEY_BASE + n`, handed out by
/// the bridge on first sight of a list name and never reused within a run.
/// One key per list because the query pool and the app's mailbox both
/// coalesce per key: under a shared key, one list's refresh would cancel
/// another's resolution in flight. The range sits below the shell's single
/// reserved keys (`REFERENCE_KEY` is `u64::MAX - 6`) and far above any
/// `TileId`-derived key. Answers never reach a tile: the bridge keeps them.
pub const WATCHLIST_KEY_BASE: QueryKey = QueryKey(u64::MAX - 0x1_0000);
/// How many keys the range holds; the top 16 values stay free for the
/// shell's single keys.
pub const WATCHLIST_KEY_COUNT: u64 = 0x1_0000 - 16;
// The range must end below the lowest single reserved key, or a list's
// answer would be routed as the shell's.
const _: () = assert!(WATCHLIST_KEY_BASE.0 + WATCHLIST_KEY_COUNT <= REFERENCE_KEY.0);

/// Whether `key` is a watchlist lane's key.
pub fn is_watchlist_key(key: QueryKey) -> bool {
    key.0 >= WATCHLIST_KEY_BASE.0 && key.0 < WATCHLIST_KEY_BASE.0 + WATCHLIST_KEY_COUNT
}

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

/// The names `Color…` may be offered for: `geode_core::colour::text_dimensions`
/// over the current datasets and derived dimensions. Cached on the shell
/// and rebuilt with [`pickable_columns`].
pub fn text_dimension_names(config: &Config) -> std::collections::BTreeSet<String> {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    geode_core::colour::text_dimensions(&schema, &dims)
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
    /// The app-level owner of the services, config directories and
    /// workspaces this window reads; see [`ShellApp`].
    app: Entity<ShellApp>,
    /// This window's identity in `app`, issued by `ShellApp::add_window`.
    key: WindowKey,
    /// The window this view is the root of. The app's watcher asks whether
    /// it is still open: a host (the bridge's closures, the quit hooks) can
    /// hold this entity past its window, and liveness is the window's.
    window_handle: gpui::AnyWindowHandle,
    matcher: Matcher,
    focus_handle: FocusHandle,
    /// State for the open command palette, or `None`. Each open rebuilds the
    /// registry/keymap/theme snapshot and reverse binding index; closing drops
    /// it. Usage history is the app's (`ShellApp::palette_usage`), shared
    /// by every window and persisted with the session.
    palette: Option<PaletteState>,
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
    /// Deferred focus restoration, armed when a reload closes the palette
    /// (`on_app_event`). `render` consumes it before painting. Waiting
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
    /// The app's shared frame (`ShellApp::frame`), cloned here so every
    /// occupant can hold it.
    frame: Entity<Frame>,
    /// The app's diagnostics state (`ShellApp::diagnostics`), fed by the app
    /// bridge and config load/reload. This window's observer drains
    /// log-level and overlay requests; the app bridge handles catalog
    /// requests.
    diagnostics: Entity<Diagnostics>,
    /// Pending occupant creation, addressed by the tile ID allocated or filled
    /// by `add_tile`. `ensure_occupants` consumes each request at that tile on
    /// the next render; simultaneous requests cannot replace one another.
    pending_tiles: BTreeMap<TileId, PendingTile>,
    /// Session tile records not yet consumed by an occupant, drained from
    /// the app's parked records at construction
    /// (`ShellApp::take_parked_records`): a restore input, not a shared
    /// service, so creating an occupant during render reads nothing
    /// through the app entity's writer.
    restored_tiles: crate::session::TileRecords,
    /// Likewise for `[pages.<kind>]` tables; a replaced page's last state
    /// goes back in here so the next save still writes it.
    restored_pages: crate::session::PageRecords,
    /// Restored records whose module factory is unavailable. `current_tiles`
    /// writes them back unchanged, preserving sessions from builds with more
    /// modules. Closing or filling their tile removes them.
    unplaced_records: crate::session::TileRecords,
    /// The frame's `(scope, grouping, as_of)` versions as of the last
    /// `on_frame_changed` — compared against the frame's
    /// current ones there to decide whether to open a fresh flip barrier.
    /// Seeded once in `new`, from the frame `ShellApp::new` has already
    /// applied a restored session's scope/slot/as-of to, so that restore is
    /// never itself mistaken for
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
    /// The frame generation emitters were last re-pulled at. An emitter
    /// composes over its lane or followed group, which move without the
    /// tile announcing anything, so the frame observer re-pulls every
    /// emitter once the generation passes this.
    last_emit_generation: u64,
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
    /// The title last given to the platform window (`sync_title`), so the
    /// event and dispatch paths that may have changed it set it only when
    /// it differs. `None` until the constructor's first set. Render never
    /// reads or sets the title.
    last_title: Option<String>,
    /// The transient stack-member list, or
    /// `None` when closed — `open_stack_list`'s own contract, the same
    /// "nothing survives a close/reopen" shape `palette`/`command_line`
    /// follow. Owns the keyboard while open (`handle_key_down`'s own
    /// branch), closed by any dispatch (`dispatch`'s own top, beside
    /// `notice`), and dropped by `render`'s generic staleness check when
    /// its tile stops being the focused member.
    stack_list: Option<stacklist::StackList>,
    /// The row menu (`tile::context_menu`), or `None` when closed. Owns
    /// the keyboard while open (`handle_key_down`'s own branch) and
    /// closes on any dispatch, the palette or a dialog opening, or a
    /// press outside it.
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
    /// Cached [`pickable_columns`] for the current datasets and dimensions.
    /// Reload rebuilds this list; startup registers per-column actions once.
    /// Columns added later remain reachable through the two-stage `frame::pick`
    /// flow even though they have no new per-column palette action.
    pickable: Vec<Pickable>,
    /// Cached [`text_dimension_names`]; rebuilt when `pickable` is.
    pub(crate) text_dims: std::collections::BTreeSet<String>,
    /// Cached [`expr_vocab`] for the current datasets and dimensions: the
    /// scope expression suggestions' columns. Reload rebuilds it beside
    /// `pickable` and re-ranks an open expression field against it.
    expr_vocab: std::rc::Rc<geode_core::scope::complete::ExprVocab>,
    /// State for the open dimension picker. Created by `picker::open` and
    /// cleared when its kind pops.
    picker: Option<picker::PickerState>,
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
    /// Scroll handle for the Scope dialog's one list (current sections,
    /// separator, filter row, saved sections). `scopedialog::view::follow_cursor`
    /// keeps the cursor's row in view after every re-derive of its rows; it
    /// outlives a close, so every opening resets its offset to the top.
    pub(crate) scope_dialog_scroll: ScrollHandle,
    /// The frame data version used to build the open as-of dialog's rows.
    /// `on_frame_changed` refreshes them only when this version changes. Set on
    /// open and unused while the dialog is closed.
    as_of_data_version: u64,
    /// State for the open scope expression dialog. Created by
    /// [`scope_expr_view::open`] and cleared when its kind pops.
    scope_expr_dialog: Option<scope_expr_view::ScopeExprState>,
    /// The Scope dialog's state while it is in the stack.
    pub(crate) scope_dialog: Option<scopedialog::view::ScopeDialogState>,
    /// State for the open choice dialog: the scope, tile-kind, column,
    /// log-level, action-value or link-group picker. Created by one of
    /// `choicedialog`'s `open_*` doors, and cleared when its kind pops.
    choice_dialog: Option<choicedialog::ChoiceDialogState>,
    /// Scroll state for the choice dialog's row list
    /// (`dialog::choice_rows`'s viewport) — the `settings_scroll` split,
    /// one dialog over.
    choice_dialog_scroll: ScrollHandle,
    /// The value-color hue stage's slider, while that stage is open:
    /// created when `Custom…` opens it (never in render), dropped with the
    /// stage and with the choice dialog.
    hue_slider: Option<choicedialog::HueSlider>,
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
        app: Entity<ShellApp>,
        key: WindowKey,
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
                    view.active_frame(cx)
                        .update(cx, |f, _| f.begin_scope_session());
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.active_frame(cx).update(cx, |f, cx| {
                        let mut s = f.scope().clone();
                        s.text = (!text.trim().is_empty()).then_some(text);
                        if f.set_scope_in_session(s) {
                            cx.notify();
                        }
                    });
                }
                InputEvent::PressEnter { .. } => {
                    view.filter_session_base = None;
                    view.active_frame(cx)
                        .update(cx, |f, _| f.end_scope_session());
                    view.focus_home(window, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.filter_session_base = None;
                    view.active_frame(cx)
                        .update(cx, |f, _| f.end_scope_session());
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
                Some(dialog::DialogKind::Scope) => {
                    if let Some(state) = view.scope_dialog.as_mut() {
                        // The text step, the definition step and a name
                        // prompt type their draft; the saved group filters
                        // while in filter mode; the screen ignores it.
                        scopedialog::view::on_query_changed(state, &query);
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
                if view.cancel_divider_drag() {
                    view.mark_session_dirty(cx);
                }
                cx.notify();
            }
        })
        .detach();

        // What the app announces to every window: a reload's per-window
        // work, a date change, and later a park. The watcher that reloads
        // is the app's (`ShellApp::spawn_watcher`); this window only reacts.
        cx.subscribe_in(&app, window, |view, _app, event, window, cx| {
            view.on_app_event(event, window, cx);
        })
        .detach();

        // The frame and the diagnostics gatherer are the app's, created and
        // seeded in `ShellApp::new` (a restored session's lanes and group
        // scopes are already applied); this window holds clones so its
        // occupants and surfaces reach them without the app entity.
        let (frame, diagnostics) = {
            let app = app.read(cx);
            (app.frame().clone(), app.diagnostics().clone())
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
        // Window-bound like the frame's observer above: the page-open drain
        // moves focus to the page.
        cx.observe_in(&diagnostics, window, |view, diagnostics, window, cx| {
            view.on_diagnostics_changed(diagnostics, window, cx);
        })
        .detach();

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
        // The restore inputs become this view's own (see the fields' docs):
        // the tile records the app parked for this window, and the page
        // tables, which the services keep none of after this.
        let (restored_tiles, restored_pages) = app.update(cx, |app, _| {
            (
                app.take_parked_records(key),
                std::mem::take(&mut app.services.restored_pages),
            )
        });
        // Gathered before the frame update: the record checks read the
        // services, which the frame's writer cannot hold beside.
        let restored_memberships: Vec<(u64, geode_core::link::Membership)> = {
            let services = app.read(cx).services();
            if restored_tiles
                .values()
                .any(|record| !record.link.is_empty())
            {
                restored_tiles
                    .iter()
                    .filter(|(id, record)| {
                        services.roster.factory(&record.kind).is_some()
                            && services.workspaces.workspace_of(TileId(**id)).is_some()
                    })
                    .map(|(id, record)| (*id, record.link))
                    .collect()
            } else {
                Vec::new()
            }
        };
        if !restored_memberships.is_empty() {
            frame.update(cx, |f, _| {
                for (id, link) in &restored_memberships {
                    let tile = TileId(*id);
                    {
                        f.follow(tile, link.follow);
                        // A loop of groups would narrow every group on it
                        // on each pass; tiles restore in id order, so the
                        // later tile's emit is the one dropped.
                        if let Some((from, to)) = f.closing_cycle(tile, link.follow, link.emit) {
                            tracing::warn!(
                                target: "geode::session",
                                "tile {id}: emit into {} dropped: {}",
                                to.letter(),
                                crate::shell::link::cycle_refusal(from, to)
                            );
                        } else {
                            f.emit(tile, link.emit);
                        }
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
            .view(
                app.read(cx)
                    .services
                    .workspaces
                    .shown(key)
                    .expect("the window was added to the workspaces before its view"),
            )
            .versions();
        let last_flip_groups = frame.read(cx).group_scope_gens();
        let last_emit_generation = frame.read(cx).generation();

        let services = app.read(cx).services();
        // The dimension pickers' column list — see
        // `pickable`'s field doc.
        let pickable = pickable_columns(&services.config);
        let text_dims = text_dimension_names(&services.config);
        let expr_vocab = std::rc::Rc::new(expr_vocab(&services.config));
        let page_entries: Vec<crate::module::PageEntry> = services.pages.entries().collect();

        // Registered with the app below, once the view is whole: the
        // session writer asks every registered view for its records.
        let weak = cx.weak_entity();
        let mut view = Self {
            app,
            key,
            window_handle: window.window_handle(),
            matcher: Matcher::default(),
            focus_handle,
            palette: None,
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
            restored_tiles,
            restored_pages,
            unplaced_records: crate::session::TileRecords::new(),
            last_flip_versions,
            last_flip_groups,
            last_emit_generation,
            occupants: HashMap::new(),
            visible_tiles: HashSet::new(),
            emit_subs: HashMap::new(),
            link_label: None,
            stack_sent: HashMap::new(),
            focused_sent: None,
            notice: None,
            stack_list: None,
            row_menu: None,
            scratch_all_tiles: HashSet::new(),
            scratch_active_tiles: HashSet::new(),
            scratch_visible_keys: Vec::new(),
            pickable,
            text_dims,
            expr_vocab,
            picker: None,
            picker_scroll: UniformListScrollHandle::new(),
            as_of_dialog: None,
            as_of_scroll: ScrollHandle::new(),
            scope_dialog_scroll: ScrollHandle::new(),
            as_of_data_version: 0,
            scope_expr_dialog: None,
            scope_dialog: None,
            choice_dialog: None,
            choice_dialog_scroll: ScrollHandle::new(),
            hue_slider: None,
            expr_scroll: ScrollHandle::new(),
            object_dialog: None,
            object_dialog_scroll: ScrollHandle::new(),
            pending_config_write: None,
            config_write_seq: 0,
            config_write_error: None,
            last_title: None,
        };
        let handle = window.window_handle();
        view.app
            .update(cx, |app, _| app.register_view(key, weak, handle));
        // After registering: the title counts this window.
        view.sync_title(window, cx);
        view
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
            DialogKind::Scope => self.scope_dialog = None,
            DialogKind::Choice => {
                self.choice_dialog = None;
                self.hue_slider = None;
            }
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
        //
        // A window the close path has deregistered (see `render`) is still
        // notified until gpui removes it. It shows no workspace, so there is
        // no lane to flip against, no field to reflect into, and the queues
        // drained below are the frame's, which every live window drains.
        if self.workspaces(cx).shown(self.key).is_none() {
            return;
        }
        let ws = self.shown_ix(cx);
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
            self.visible_tile_keys(&mut keys, cx);
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
                    // answered an older frame. Another window answering
                    // this same change (same generation) joins it instead
                    // of replacing it, or its tiles would no longer be
                    // awaited and the two windows would flip apart.
                    f.join_flip(awaited, Instant::now());
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
            let as_of = self.target_frame(cx).read(cx).as_of().clone();
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
            && let Some(dir) = self.user_dir(cx)
        {
            crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
                if let Err(e) = crate::frame::persist_slot_to_user_config(&dir, slot, &grouping) {
                    tracing::warn!(target: "geode::config", "{e}");
                }
            })
            .detach();
        }
        // Drain scope saves in the background, just like grouping saves. The
        // Scope dialog's save prompt writes through `config_write` directly;
        // this handles requests queued on the frame itself.
        if let Some((name, scope)) = frame.update(cx, |f, _| f.take_pending_scope_persist())
            && let Some(dir) = self.user_dir(cx)
        {
            crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
                if let Err(e) = crate::frame::persist_scope_to_user_config(&dir, &name, &scope) {
                    tracing::warn!(target: "geode::config", "{e}");
                }
            })
            .detach();
        }
        // Whole-object config edits queued by a module tile join the object
        // dialogs' pending batch: one debounce, one write, one reload. The
        // tile that asked hears of a fork or a refusal through the frame;
        // with no user directory the refusal also shows on the status bar.
        let edits = frame.update(cx, |f, _| f.take_pending_config_edits());
        if !edits.is_empty() {
            let mut origins: Vec<TileId> = edits.iter().filter_map(|e| e.origin).collect();
            let posted: Vec<(TileId, crate::frame::TileNotice)> =
                match objectdialog::apply::queue_edits(self, edits, cx) {
                    Ok(notices) => notices
                        .into_iter()
                        .map(|(t, n)| (t, crate::frame::TileNotice::Forked(n)))
                        .collect(),
                    Err(e) => {
                        self.config_write_error = Some(e.clone().into());
                        cx.notify();
                        // One refusal per tile, however many of its edits
                        // the drain refused together.
                        origins.sort_unstable();
                        origins.dedup();
                        origins
                            .into_iter()
                            .map(|t| (t, crate::frame::TileNotice::Refused(e.clone())))
                            .collect()
                    }
                };
            if !posted.is_empty() {
                // Deferred: this runs inside the frame's own observer, and a
                // tile reads its notices on the frame notification below.
                // That notification re-enters this observer; it is harmless
                // because the edit queue was drained above, and posting moves
                // no frame version, so it opens no flip and starts no
                // requery.
                let frame = frame.clone();
                cx.defer(move |cx| {
                    frame.update(cx, |f, cx| {
                        for (tile, notice) in posted {
                            f.post_tile_notice(tile, notice);
                        }
                        cx.notify();
                    })
                });
            }
        }
        // Emitters compose over their lane or followed group, which move
        // without the tile hearing it. After this observer: the pull reads
        // tiles and writes the frame.
        let weak = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = weak.update(cx, |view, cx| view.repull_emitters(cx));
        });
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
                .active_frame(cx)
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
        // would offer a chain that no longer exists, and the Scope dialog,
        // whose rows are the lane's scope. Last, after every `frame.update`
        // above, so the key carries the final generation.
        if self
            .object_dialog
            .as_ref()
            .is_some_and(|state| state.domain.applies_from_browse())
            || self.scope_dialog.is_some()
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
        // A window the close path has deregistered (see `render`) is still
        // notified until gpui removes it; it shows no workspace, and a page
        // opened here would read the shown lane. Another window drains the
        // requests.
        if self.workspaces(cx).shown(self.key).is_none() {
            return;
        }
        let (pending_level, pending_overlay, pending_page) = diagnostics.update(cx, |d, _cx| {
            (
                d.take_pending_level(),
                d.take_pending_overlay_toggle(),
                d.take_pending_diagnostics_page(),
            )
        });
        if let Some((target, level)) = pending_level {
            let levels = diagnostics.read(cx).levels.clone();
            if let Some(log) = &self.services(cx).log
                && let Err(e) = log.control.set(&levels)
            {
                tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
            }
            if let Some(dir) = self.user_dir(cx) {
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

    /// The app entity this window reads its services from.
    pub fn app(&self) -> &Entity<ShellApp> {
        &self.app
    }

    /// This window's key in [`Self::app`].
    pub fn key(&self) -> WindowKey {
        self.key
    }

    /// The window this view is the root of, open or closed.
    pub fn window_handle(&self) -> gpui::AnyWindowHandle {
        self.window_handle
    }

    /// The services every window shares. The reference borrows `cx`, not
    /// `self`, so a caller may hold it beside a mutable borrow of its own
    /// fields; it may not hold it across a mutable use of `cx`.
    pub(crate) fn services<'a>(&self, cx: &'a App) -> &'a ShellServices {
        &self.app.read(cx).services
    }

    /// Change the app's own state in place: the settings a settings row
    /// steps (`font_size` and its siblings). An event, dispatch or
    /// constructor path only, never render.
    pub(crate) fn with_app<R>(&self, cx: &mut App, f: impl FnOnce(&mut ShellApp) -> R) -> R {
        self.app.update(cx, |app, _| f(app))
    }

    /// Change the shared services in place. Every write goes through here
    /// or one of the narrower `with_*` siblings; nothing holds a mutable
    /// reference to the services outside the closure.
    pub(crate) fn with_services<R>(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut ShellServices) -> R,
    ) -> R {
        self.with_app(cx, |app| f(&mut app.services))
    }

    /// A layout change in this window awaits the app's next periodic
    /// snapshot. An event, dispatch or constructor path only: the draw
    /// pass never updates the app entity, so a render-time cancellation
    /// defers its mark (`render.rs`).
    pub(crate) fn mark_session_dirty(&self, cx: &mut App) {
        self.app.update(cx, |app, _| app.mark_session_dirty());
    }

    pub(crate) fn workspaces<'a>(&self, cx: &'a App) -> &'a Workspaces {
        &self.services(cx).workspaces
    }

    pub(crate) fn with_workspaces<R>(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut Workspaces) -> R,
    ) -> R {
        self.with_services(cx, |s| f(&mut s.workspaces))
    }

    /// The workspace this window shows.
    pub(crate) fn workspace<'a>(&self, cx: &'a App) -> &'a Workspace {
        self.workspaces(cx).workspace(self.key)
    }

    pub(crate) fn with_workspace<R>(&self, cx: &mut App, f: impl FnOnce(&mut Workspace) -> R) -> R {
        let key = self.key;
        self.with_workspaces(cx, |w| f(w.workspace_mut(key)))
    }

    /// The current config, for the app bridge to read the new `views` doc
    /// out of after a `ShellEvent::ConfigReloaded` — `geode-app` is
    /// the only crate allowed to touch `geode-data`, so it needs to reach
    /// the reloaded config through the shell rather than reloading it a
    /// second time itself.
    pub fn config<'a>(&self, cx: &'a App) -> &'a Config {
        &self.services(cx).config
    }

    /// The user config directory, the one layer runtime writes target;
    /// `None` makes every persist path a silent no-op.
    pub(crate) fn user_dir(&self, cx: &App) -> Option<PathBuf> {
        self.app.read(cx).user_dir.clone()
    }

    /// The shared frame entity every occupant holds. Lane state (scope,
    /// grouping, as-of) is reached through `target_frame` or
    /// [`Self::active_frame`], which name the workspace.
    pub fn frame(&self) -> &Entity<Frame> {
        &self.frame
    }

    /// The index of the workspace this window shows.
    pub(crate) fn shown_ix(&self, cx: &App) -> WorkspaceIx {
        self.workspaces(cx)
            .shown(self.key)
            .expect("a view's window is open on the workspaces")
    }

    /// Give the platform window its title (`ShellApp::window_title`:
    /// `Geode`, or `Geode · N` once two windows are registered) when it
    /// differs from the one last set. Called where the title can change
    /// and a `Window` is at hand: the end of the constructor, the
    /// `WindowsChanged` event, and this window's own switch and tear-out
    /// (`after_shown_changed`). Never from render: a title is set, not
    /// painted, and a per-frame compare would still format per frame. A
    /// deregistered window (see `render`) is left as it was.
    fn sync_title(&mut self, window: &mut Window, cx: &App) {
        if self.workspaces(cx).shown(self.key).is_none() {
            return;
        }
        let title = self.app.read(cx).window_title(self.key);
        if self.last_title.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.last_title = Some(title);
        }
    }

    /// The title last set on the platform window, or `None` before the
    /// constructor's first set — test reach for the title rule.
    #[cfg(any(test, feature = "test-support"))]
    pub fn last_title(&self) -> Option<&str> {
        self.last_title.as_deref()
    }

    pub(crate) fn frame_at(&self, ws: WorkspaceIx) -> FrameRef {
        FrameRef::new(self.frame.clone(), ws)
    }

    /// The lane every shell surface reads and writes: the workspace the
    /// open modal stack was opened from, else the active one. A dialog
    /// therefore commits where it was opened, whatever is active later.
    pub(crate) fn target_frame(&self, cx: &App) -> FrameRef {
        let ws = self
            .modals
            .first()
            .map(|m| m.workspace)
            .unwrap_or_else(|| self.shown_ix(cx));
        self.frame_at(ws)
    }

    /// Load saved scope `name` into [`Self::target_frame`]'s lane through
    /// `load_scope` (so it is one undoable `set_scope` step and honours a
    /// workspace pin), notifying on a change. The one path the
    /// `scope::<name>` actions, the palette's Scope rows and the Scope dialog's
    /// saved rows take. `Err` when no
    /// saved scope has that name; `Ok(false)` when it is already current.
    pub(crate) fn load_saved_scope(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        self.target_frame(cx).update(cx, |f, cx| {
            let before = f.generation();
            let loaded = f.load_scope(name);
            // Loading the scope in force changes only the lane's provenance;
            // observers keyed on the generation still have to hear it.
            if f.generation() != before {
                cx.notify();
            }
            loaded
        })
    }

    /// The shown workspace's frame, for the app's catalog as-of.
    pub fn active_frame(&self, cx: &App) -> FrameRef {
        self.frame_at(self.shown_ix(cx))
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
    /// to the open expression field's suggestions. `ACTION_KEY` routes to an open
    /// action value choice. `PICKER_KEY` replies reach the dimension
    /// picker only if it is open in Values stage and both column and latest
    /// request tag match; any other key is dropped. Stale replies cause no mutation or notification.
    pub fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        if outcome.key == EXPR_KEY {
            expr_suggest::deliver(self, outcome, cx);
            return;
        }
        if outcome.key == ACTION_KEY {
            choicedialog::deliver_action_values(self, outcome, cx);
            return;
        }
        // Only the picker's own key reaches it: a tile's tag counter can
        // match the picker's, and its values would then fill the picker.
        if outcome.key != PICKER_KEY {
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
