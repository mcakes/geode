//! The app-level owner of [`ShellServices`]: one entity every window's
//! [`ShellView`] reads through its [`WindowKey`].
//!
//! A window never owns config, the keymap, the registry, the theme or the
//! workspaces; it reads them from here and writes them through
//! `ShellView::with_services` and its narrower siblings. One owner means a
//! second window sees the same config revision and the same tile
//! allocator as the first, with nothing to reconcile between them.
//!
//! The shared [`Frame`] and the [`Diagnostics`] gatherer are created here
//! too, and a restored session's lanes and group scopes are applied to the
//! frame once, before any window exists: a second window finds the same
//! pins and scopes the first does, and restores nothing of its own.
//!
//! The session is the app's as well: its dirty flag, the baselines the
//! periodic writer compares against, the palette's usage history and the
//! tile records no window has taken (`parked_tiles`). One writer
//! (`session_io.rs`) snapshots every live window's occupants plus the
//! parked records, so a tile hosted by any window, or by none, reaches the
//! file. Render never updates this entity: a window marks the session
//! dirty from its event, dispatch and constructor paths only.
//!
//! The app-wide settings a reload or a settings row changes (`font_size`,
//! `find_style`, `add_direction`, `line_numbers`, `default_source`,
//! `link_include_tile_filter`), the reload state (`config_revision`, the
//! last outcome and its status text, the restart notice and the baselines
//! it compares against) and the cached date are the app's too, read by
//! every window through the accessors below. One watcher loop
//! (`hot_reload::spawn_watcher`), started here, sweeps the frame, feeds
//! diagnostics, refreshes the date, writes the session and reloads
//! config; a window hears of a reload through [`ShellEvent::Reloaded`]
//! and of a date change through [`ShellEvent::DayChanged`], and reacts to
//! each in its own `on_app_event`.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, EventEmitter, SharedString, WeakEntity,
};

use super::{ShellEvent, ShellServices, ShellView, hot_reload};
use crate::diagnostics::Diagnostics;
use crate::fontsize::FontSize;
use crate::frame::Frame;
use crate::linenumbers::LineNumbers;
use crate::module::Delivery;
use crate::palette_usage::PaletteUsage;
use crate::reload;
use crate::session::{PageRecords, TileRecords};
use crate::tileadd::AddDirection;
use crate::tiling::{Added, Switch, TileId, WindowKey, WorkspaceIx};
use crate::vimfind::FindStyle;
use geode_core::config::{Config, LayerDoc};
use geode_core::positions::CommandOutcome;
use geode_core::query::{DistinctOutcome, QueryKey};

impl EventEmitter<ShellEvent> for ShellApp {}

/// Where a new window should appear. The opener (the composition root's
/// closure) maps it to platform window options; the app never reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    /// Next to the named window, as a verb dispatched there asks.
    Beside(WindowKey),
    /// Where a restored session recorded the window, when it did.
    Restored {
        display: Option<u64>,
        bounds: Option<gpui::Bounds<gpui::Pixels>>,
        maximized: bool,
    },
    /// Wherever the platform puts a window by default.
    Default,
}

/// What [`ShellApp::open_window`] opens: the workspace the window shows
/// first and where it appears.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSpec {
    pub workspace: WorkspaceIx,
    pub placement: Placement,
}

/// The one routine that opens a platform window over a `ShellView` for a
/// key the app has already reserved, installed once by the composition
/// root ([`ShellApp::set_window_opener`]) and by the test harness. It runs
/// with no lease on the app: the view it builds updates the app itself
/// (`register_view`, `take_parked_records`). `None` means no window was
/// opened and the caller rolls the reservation back.
pub type WindowOpener = Rc<dyn Fn(WindowKey, &WindowSpec, &mut App) -> Option<AnyWindowHandle>>;

/// Where an asynchronous answer goes ([`ShellApp::route`]). A keyed
/// answer has one home, the window hosting its tile's workspace, shown or
/// hidden; a keyless answer and one under a shell-owned key (a picker, the
/// diagnostics page, the reference and watchlist lanes) concern every
/// window, each of which checks its own tag; an answer for a tile no
/// window hosts (parked by a close, awaiting the next window to show its
/// index) has nowhere to go and is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Window(WindowKey),
    Broadcast,
    Drop,
}

/// Why a window was not opened. Each leaves the app as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRefusal {
    /// No index is free for the window, or the asked-for index is shown
    /// by another window.
    NoFreeWorkspace,
    /// No opener is installed; nothing can open a window.
    NoOpener,
    /// The opener opened nothing, or opened a window whose view did not
    /// register for the key.
    OpenerFailed,
}

pub struct ShellApp {
    pub(crate) services: ShellServices,
    /// Config directories read by each watcher poll; `None` layers are
    /// skipped by the scan and every persist path is a silent no-op
    /// without a user directory.
    pub(crate) desk_dir: Option<PathBuf>,
    pub(crate) user_dir: Option<PathBuf>,
    /// The shared frame: one set of lanes, link groups and version counters
    /// for every window and every occupant.
    pub(super) frame: Entity<Frame>,
    /// Shared diagnostics state, fed by the app bridge and config
    /// load/reload. Each window observes it for the requests its surfaces
    /// drain.
    pub(super) diagnostics: Entity<Diagnostics>,
    /// The last key issued by [`Self::reserve`]; keys count up from 1
    /// and are never reused.
    next_window: u64,
    /// The window registry: every open window's key and platform handle,
    /// in opening order. An entry is made by [`Self::register_view`] at
    /// the end of `ShellView::new` and removed by [`Self::window_closed`]
    /// from the close path; nothing else adds or drops one. This list is
    /// what "open" means to the app: the watcher runs, the session is
    /// saved and the title names an index by it, not by `cx.windows()`
    /// and not by whether a view's entity is still alive, since the
    /// bridge's closures and the quit hooks hold a view past its window.
    windows: Vec<(WindowKey, AnyWindowHandle)>,
    /// Every registered window's view, kept beside [`Self::windows`] and
    /// removed with it. Weak, so this owner never keeps a view alive;
    /// [`Self::views`] skips a handle whose view is gone.
    pub(super) views: Vec<(WindowKey, WeakEntity<ShellView>)>,
    /// Whether any view has registered yet. The watcher and the bridge's
    /// lanes end once the registry is empty, but not before the first
    /// window registers: both start with this entity, ahead of its first
    /// window ([`Self::any_window_open`]).
    has_registered_view: bool,
    /// The last distinct-request tag issued by [`Self::next_tag`]. One
    /// counter for every window's pickers, expression fields and action
    /// value choices: their requests share a few reserved keys and their
    /// answers are broadcast, so a tag must never repeat across windows or
    /// one window's reply would fill another's picker.
    next_tag: u64,
    /// What opens a window for a reserved key, installed once by the
    /// composition root. `None` until then: `open_window` refuses
    /// `NoOpener` before it reserves anything.
    window_opener: Option<WindowOpener>,
    /// The effective UI font size (small/medium/large — `[ui] font_size`).
    /// Applied as each window's rem size at the top of its `render`, the
    /// one place with a `Window` on every path that can change it
    /// (startup, the settings control via `settings_view::set_font_size`,
    /// config hot reload) — see the `fontsize` module doc.
    pub(super) font_size: FontSize,
    /// The configured `/`-find style (`[ui] find_style`). Resolved at startup,
    /// on reload, and by the settings control. Each new `/` prompt chooses
    /// the tile's Vim event stream or the shared Fzf picker from this value.
    pub(super) find_style: FindStyle,
    /// `[tiles] add`: resolved at startup,
    /// re-derived on hot reload, stepped by the settings row.
    pub(super) add_direction: AddDirection,
    /// `[ui] line_numbers`: same lifecycle as
    /// `add_direction`, and additionally published as the
    /// `linenumbers::UiSettings` global on every change so a module
    /// (the blotter) can read and observe it — see that module's doc.
    pub(super) line_numbers: LineNumbers,
    /// `[timeseries] default_source`: same
    /// lifecycle as `line_numbers`, published with it as the
    /// `series::SeriesSettings` global so a timeseries tile can read the
    /// source `:add` means without a path to the shell.
    pub(super) default_source: Option<String>,
    /// Whether a posting composes the emitter's own `:filter` layer in:
    /// `[links] include_tile_filter`, seeded from config, written by
    /// `ShellView::set_link_filter` and hot reload, each of which re-pulls
    /// every emitter on a change.
    pub(super) link_include_tile_filter: bool,
    /// Today on the configured clock, refreshed by the watcher's tick
    /// (`DayChanged` tells every window) and by a reload that changes the
    /// clock. A window paints this cached date in its scope bar instead of
    /// reading the clock on every repaint.
    pub(super) today: chrono::NaiveDate,
    /// Last config-directory snapshot. The first background scan replaces the
    /// empty initial value without triggering a reload. Later scans compare
    /// against it about every 500 ms. Scanning never runs on the UI thread.
    pub(super) last_snapshot: reload::Snapshot,
    /// The result of the last reload attempt, `Unchanged` until the first
    /// one runs. Drives the status bar's reload indicator.
    pub(super) last_reload: reload::ReloadOutcome,
    /// The rejected-reload segment, prepared when `last_reload` is set.
    pub(super) reload_status: Option<SharedString>,
    /// Bumped by every applied reload — the one place `services.config` and
    /// `services.keymap` are replaced. Config dialog rows derived from either are
    /// keyed by it (`crate::prepared`).
    pub(super) config_revision: u64,
    /// Restart notice for config that differs from the running data service.
    /// Reload compares restart-sensitive settings against their startup
    /// baselines; restoring those values clears the notice. The status bar
    /// paints it and the app bridge receives `ShellEvent::RestartRequired`.
    pub(super) restart_required: Option<SharedString>,
    /// The `sources` layered doc the running `DataService` was actually
    /// built from — captured once here at construction, since a reload
    /// never rebuilds the data engine (see `apply_reload`'s doc comment).
    /// `apply_reload` compares each freshly loaded config's `sources` doc
    /// against this baseline, not against the previous reload's config,
    /// so reverting an edit back to the value the service was built from
    /// clears `restart_required`: the message means "what's on disk no
    /// longer matches what's running", and a revert makes that false
    /// again.
    pub(super) sources_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `datasets` doc.
    pub(super) datasets_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `egress` doc: nothing reloads a
    /// resolved target's transport live, so `egress.toml` is restart-
    /// required exactly as `sources.toml` is.
    pub(super) egress_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `positions` doc: the position service is resolved once at
    /// startup, so `positions.toml` is restart-required exactly as
    /// `egress.toml` is.
    pub(super) positions_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `panels` doc: panels become tile kinds once at startup, so
    /// `panels.toml` is restart-required.
    pub(super) panels_baseline: Vec<LayerDoc>,
    /// The same, for the `scenarios` doc: each accepted shape becomes a
    /// tile kind (and its derived view a service view) once at startup.
    pub(super) scenarios_baseline: Vec<LayerDoc>,
    /// The startup `[pricing] adapter` value used to build the data engine.
    /// Reload compares against this baseline to determine whether a restart is
    /// required. The rest of `[pricing]`, including `refresh`, remains live.
    pub(super) pricing_baseline: Option<toml::Value>,
    /// The startup `[vol] model` value used to build the data engine's vol
    /// worker, compared on reload exactly as `pricing_baseline` is.
    pub(super) vol_baseline: Option<toml::Value>,
    /// Tile records no window holds an occupant for: the restored
    /// session's until a window takes them, and the records a window parks
    /// when a workspace it hosted leaves for another window
    /// (`ShellView::park_workspace`). A window takes the ones it hosts
    /// through [`Self::take_parked_records`], at construction and on the
    /// event and dispatch paths that make it host more; whatever remains
    /// here is written back by every save, after the live records, so a
    /// tile no window hosts keeps its module and state across a restart.
    pub(super) parked_tiles: TileRecords,
    /// How often and how recently each palette row was chosen
    /// (`crate::palette_usage`): read once per palette open to rank the
    /// rows (`PaletteState::with_usage`), written by every palette
    /// dispatch but `palette::toggle`'s own row, and persisted as
    /// `session.toml`'s `[palette.usage]` table through the same
    /// coalesced flush the layout rides. Seeded from the restored session.
    pub(super) palette_usage: PaletteUsage,
    /// Bumped by every `palette_usage` mutation; `take_dirty_session_write`
    /// compares it against `last_palette_usage_written`, the same shape
    /// as `last_frame_generation_written`, so a palette dispatch that
    /// mutates nothing else still reaches the flush.
    pub(super) palette_usage_version: u64,
    pub(super) last_palette_usage_written: u64,
    /// Layout changes awaiting snapshot extraction, set by any window
    /// through [`Self::mark_session_dirty`]. The periodic watcher clears
    /// this flag before serialization and writes in the background; it is
    /// not a record of whether the disk write succeeded.
    pub(super) session_dirty: bool,
    /// Tile records from the last successfully serialized periodic snapshot,
    /// initially empty. Compared every tick to catch module-only state changes.
    /// Updated before disk I/O; a failed write does not reset this baseline.
    pub(super) last_tiles_written: TileRecords,
    /// Page records from the last successfully serialized periodic snapshot,
    /// initially empty. Compared every tick like `last_tiles_written`, since
    /// a page's state changes do not set the layout flag.
    pub(super) last_pages_written: PageRecords,
    /// The frame's generation counter captured by the last successfully
    /// serialized periodic snapshot, initially zero. Every scope, grouping,
    /// as-of, pin, and unpin change in any lane advances it, so it detects
    /// frame-only changes; updated before the disk write completes.
    pub(super) last_frame_generation_written: u64,
    /// The last periodic snapshot handed to the writer, as text WITHOUT the
    /// link groups' scopes (`[links]`); `None` before the first. A group's
    /// scope moves with an emitting tile's cursor and advances the frame
    /// generation, so a dirty check can produce a snapshot that differs
    /// from the last only there; that snapshot is not written. The file's
    /// text carries the groups' scopes whenever a snapshot is written.
    pub(super) last_session_text: Option<String>,
}

impl ShellApp {
    pub fn new(
        mut services: ShellServices,
        desk_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        // The shared frame: built from whatever `[groupings]`/
        // `[scopes]` (plus the `datasets`/`dimensions` docs they validate
        // against) config resolved to — see `hot_reload::rebuild_slots`/
        // `rebuild_saved_scopes`, shared with `apply_reload`'s own
        // rebuilds.
        let frame = {
            let config = &services.config;
            let slots = hot_reload::rebuild_slots(config);
            // Report saved-scope diagnostics here. Startup action registration
            // loads the same scopes with reporting disabled to avoid duplicate logs.
            let saved = hot_reload::rebuild_saved_scopes(config, true);
            let named = hot_reload::rebuild_named_expressions(config);
            cx.new(|_| {
                let mut frame = Frame::new(slots, saved, user_dir.clone());
                frame.replace_named_expressions(named);
                frame
            })
        };

        // The diagnostics gatherer, created alongside the frame. Seeded
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
        // `apply_reload` so the section is consistent before and after
        // reload.
        let (clock, clock_diagnostics) = geode_core::clock::Clock::from_config(&services.config);
        let startup_diagnostics = {
            let cfg = &services.config;
            let mut diags = cfg.diagnostics.clone();
            diags.extend(crate::defaults::mod_alias_from_config(cfg).1);
            diags.extend(crate::defaults::modules_default_diagnostic(cfg));
            // Warn when the default timeseries source names no fetch source.
            diags.extend(crate::series::default_source_diagnostic(cfg));
            diags.extend(clock_diagnostics);
            diags.extend(services.keymap_diagnostics.iter().cloned());
            diags.extend(services.composition_diagnostics.iter().cloned());
            diags
        };
        diagnostics.update(cx, |d, _cx| {
            d.note_config(startup_diagnostics, std::time::SystemTime::now());
        });

        // Restore frame state. The restore inputs are taken out of the
        // services: they are consumed here, once, and nothing reads them
        // after. Clearing scope history removes the undo entry created by
        // `set_scope`, so startup does not offer an undo back to the
        // initial empty scope.
        if let Some(record) = services.restored_frame.take() {
            // `[frame]` is the shared lane's record; pinned lanes restore
            // from their own workspace records.
            frame.update(cx, |f, _cx| {
                let mut s = f.shared_mut();
                s.set_scope(record.scope);
                s.set_active_slot(record.active_slot);
                if let Some(chain) = record.ad_hoc {
                    s.restore_ad_hoc(chain, record.ad_hoc_active);
                }
                if !s.restore_loaded_from(record.loaded_from.clone()) {
                    tracing::warn!(
                        target: "geode::session",
                        "restored scope provenance dropped: no saved scope '{}'",
                        record.loaded_from.as_deref().unwrap_or_default()
                    );
                }
                s.set_as_of(record.as_of);
                s.clear_history();
            });
        }
        // Each restored pinned workspace gets its own lane. Clearing history
        // keeps startup from offering an undo back to the empty scope; this
        // runs before a window seeds its flip baselines, so a restored lane
        // never reads as "just changed".
        for (ws, record) in std::mem::take(&mut services.restored_pinned) {
            // A pin on a workspace the layout lacks would surface as an
            // unexpected pin if that workspace were created later. Both come
            // from one session read, so this only guards a hand-assembled
            // `ShellServices`.
            let present = services.workspaces.spaces().any(|(ix, _)| ix == ws.get());
            if !present {
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
                // `pin` copied the shared lane's provenance; the record names
                // this lane's own, and a record without one clears the copy.
                if !lane.restore_loaded_from(record.loaded_from.clone()) {
                    tracing::warn!(
                        target: "geode::session",
                        "restored scope provenance dropped: no saved scope '{}'",
                        record.loaded_from.as_deref().unwrap_or_default()
                    );
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
        // membership is applied and before any window seeds its flip
        // baselines, and nothing is notified: a restored follower's first
        // query is already scoped by its group, with no emitter needed to
        // post it again, and the restored scope is not read as a change.
        // Lost, a follower that showed one underlying would show the whole
        // book under the same chip.
        let restored_links = std::mem::take(&mut services.restored_links);
        if restored_links.iter().any(|scope| !scope.is_empty()) {
            frame.update(cx, |f, _| {
                for group in geode_core::link::Group::ALL {
                    let scope = &restored_links[group.index()];
                    if !scope.is_empty() {
                        f.restore_group_scope(group, scope.clone());
                    }
                }
            });
        }

        // The session's own inputs, taken the same way: the tile records
        // wait here for the window that hosts each tile, and the palette's
        // history becomes the live one.
        let parked_tiles = std::mem::take(&mut services.restored_tiles);
        let palette_usage = std::mem::take(&mut services.restored_palette_usage);

        // The app-wide settings, each re-derived by `apply_reload`, and
        // the globals that mirror them: `UiSettings`, `SeriesSettings`,
        // `AppClock` and `Chords` (see `shell.md`'s six globals). Set here,
        // before any window, so a second window opening resets nothing.
        let config = &services.config;
        let font_size = FontSize::from_config(config);
        let find_style = FindStyle::from_config(config);
        let add_direction = AddDirection::from_config(config);
        let line_numbers = LineNumbers::from_config(config);
        cx.set_global(crate::linenumbers::UiSettings { line_numbers });
        let link_include_tile_filter = crate::linkfilter::from_config(config);
        // `[timeseries] default_source` plus the fetch sources it names;
        // the settings row writes both through `ShellView::set_default_source`.
        let series = crate::series::SeriesSettings::from_config(config);
        let default_source = series.default_source.clone();
        cx.set_global(series);
        cx.set_global(crate::clock::AppClock(clock));
        let today = clock.today(chrono::Utc::now());
        // Empty until the bridge's first live reference answer: a module
        // reading it before then sees no table rather than a missing global.
        cx.set_global(crate::reference::ReferenceGlobal::default());
        // Likewise empty until the bridge's first watchlist resolution.
        cx.set_global(crate::watchlist::WatchlistGlobal::default());
        // Bindings for module tooltip chord lookup through `tips::Chords`.
        let bindings = services.keymap.bindings().to_vec();
        cx.set_global(crate::tips::Chords(Arc::new(bindings)));

        // The docs the data engine actually starts with — see
        // `sources_baseline`'s field doc.
        let sources_baseline = config.layered_docs("sources").to_vec();
        let datasets_baseline = config.layered_docs("datasets").to_vec();
        let egress_baseline = config.layered_docs("egress").to_vec();
        let positions_baseline = config.layered_docs("positions").to_vec();
        let panels_baseline = config.layered_docs(geode_core::panel::PANELS_DOC).to_vec();
        let scenarios_baseline = config
            .layered_docs(geode_core::scenario::SCENARIOS_DOC)
            .to_vec();
        // Same reasoning, for the `[pricing] adapter` key the data
        // engine's pricer was chosen from — see
        // `pricing_baseline`'s field doc.
        let pricing_baseline = config.get("app", "pricing.adapter").cloned();
        // And for the `[vol] model` key — see `vol_baseline`'s field doc.
        let vol_baseline = config.get("app", "vol.model").cloned();

        let mut app = Self {
            services,
            desk_dir,
            user_dir,
            frame,
            diagnostics,
            next_window: 0,
            windows: Vec::new(),
            views: Vec::new(),
            has_registered_view: false,
            next_tag: 0,
            window_opener: None,
            font_size,
            find_style,
            add_direction,
            line_numbers,
            default_source,
            link_include_tile_filter,
            today,
            last_snapshot: reload::Snapshot::default(),
            last_reload: reload::ReloadOutcome::Unchanged,
            reload_status: None,
            config_revision: 0,
            restart_required: None,
            sources_baseline,
            datasets_baseline,
            egress_baseline,
            positions_baseline,
            panels_baseline,
            scenarios_baseline,
            pricing_baseline,
            vol_baseline,
            parked_tiles,
            palette_usage,
            palette_usage_version: 0,
            last_palette_usage_written: 0,
            session_dirty: false,
            last_tiles_written: TileRecords::new(),
            last_pages_written: PageRecords::new(),
            last_frame_generation_written: 0,
            last_session_text: None,
        };
        app.spawn_watcher(cx);
        app
    }

    /// Issue the next window key and open it on a workspace, for a caller
    /// that builds the window itself (the composition root's first
    /// window, the fixtures that open a window by hand). `index` names
    /// the workspace; `None` picks the session's initial index for the
    /// first window and the lowest index no window shows after that.
    /// [`Self::open_window`] reserves through the same [`Self::reserve`].
    ///
    /// A refusal is a programming error here: the initial index is free
    /// while no window is open, and `lowest_free` is free by definition.
    /// A caller asking for an index another window shows is expected to
    /// have checked `Workspaces::shown_in` first; `open_window` refuses
    /// that case instead of panicking.
    pub fn add_window(&mut self, index: Option<WorkspaceIx>, cx: &mut Context<Self>) -> WindowKey {
        let workspaces = &self.services.workspaces;
        let n = match index {
            Some(ix) => ix.get(),
            None if workspaces.windows().next().is_none() => workspaces.initial_index(),
            None => workspaces
                .lowest_free()
                .expect("nine windows already show every workspace")
                .get(),
        };
        self.reserve(n, cx)
            .unwrap_or_else(|| panic!("workspace {n} is shown by another window"))
    }

    /// The one place a key is issued and an index reserved for it: open
    /// index `n` in `Workspaces` under the next key. `None`, with the key
    /// counter and the workspaces untouched, when `n` is outside 1–9 or
    /// shown by another window.
    ///
    /// An index another window hosted hidden (`Added::TakenFrom`) leaves
    /// it through the same park a `Moved` switch uses: that window hears
    /// [`ShellEvent::Park`] and parks what it built for the index, and
    /// the new window pulls the records when `Parked` follows. The event
    /// is delivered when the outermost update's effects flush, after the
    /// new window's view exists to hear the answer.
    fn reserve(&mut self, n: u8, cx: &mut Context<Self>) -> Option<WindowKey> {
        let key = WindowKey(self.next_window + 1);
        let added = self.services.workspaces.add_window(key, n)?;
        self.next_window = key.0;
        if let Added::TakenFrom(host) = added {
            let workspace = WorkspaceIx::new(n).expect("add_window accepted the index");
            cx.emit(ShellEvent::Park {
                window: host,
                workspace,
            });
        }
        Some(key)
    }

    /// Install what opens windows for reserved keys. Set once by the
    /// composition root before any verb can run; a later call replaces
    /// the opener for the windows opened after it.
    pub fn set_window_opener(&mut self, opener: WindowOpener) {
        self.window_opener = Some(opener);
    }

    /// Open a window showing `spec.workspace`, in three steps. Reserve,
    /// under one app update: issue the key and open the index in
    /// `Workspaces` ([`Self::reserve`]), refusing `NoFreeWorkspace` with
    /// nothing changed when the index is shown elsewhere. Open, with no
    /// lease on the app: the opener builds the platform window and its
    /// `ShellView`, which updates the app itself (`register_view`,
    /// `take_parked_records`); an update of a leased entity is refused,
    /// so this cannot run inside the reserve step. Confirm, under a
    /// second update: the registry must now list the key (the view
    /// registered it), and `WindowsChanged` tells every window the count
    /// moved; otherwise the reservation is rolled back
    /// (`Workspaces::remove_window`) and `OpenerFailed` is returned. With
    /// no opener installed, `NoOpener` is refused before anything is
    /// reserved.
    ///
    /// An associated function rather than a method for the lease reason
    /// above: the caller holds no `&mut ShellApp` while the opener runs.
    pub fn open_window(
        app: &Entity<ShellApp>,
        spec: WindowSpec,
        cx: &mut App,
    ) -> Result<WindowKey, OpenRefusal> {
        let Some(opener) = app.read(cx).window_opener.clone() else {
            return Err(OpenRefusal::NoOpener);
        };
        let key = app.update(cx, |app, cx| {
            app.reserve(spec.workspace.get(), cx)
                .ok_or(OpenRefusal::NoFreeWorkspace)
        })?;
        let opened = opener(key, &spec, cx);
        app.update(cx, |app, cx| app.confirm_open(key, opened.is_some(), cx))?;
        Ok(key)
    }

    /// Open a new window on the lowest index no window shows, placed
    /// beside the window that asked. `NoFreeWorkspace` once nine windows
    /// show every index.
    pub fn open_fresh(
        app: &Entity<ShellApp>,
        beside: WindowKey,
        cx: &mut App,
    ) -> Result<WindowKey, OpenRefusal> {
        let Some(workspace) = app.read(cx).services.workspaces.lowest_free() else {
            return Err(OpenRefusal::NoFreeWorkspace);
        };
        Self::open_window(
            app,
            WindowSpec {
                workspace,
                placement: Placement::Beside(beside),
            },
            cx,
        )
    }

    /// Move the workspace window `from` shows into a new window beside
    /// it, and show the lowest free index in `from` instead. Refused with
    /// nothing changed when no index is free (`from`'s own index is still
    /// shown, so it is not free) or `from` is not open.
    ///
    /// Reserve, under one app update: switch `from` to the free index
    /// (always `Shown` or `Moved`: nobody shows it; a `Moved` parks it
    /// out of the window that hid it, as any switch does), then reserve
    /// the index `from` showed for the new key, which `Workspaces`
    /// answers `TakenFrom(from)`: `from` is told to park its occupants
    /// for it through the ordinary [`ShellEvent::Park`], and the new
    /// window rebuilds them. Open and confirm as [`Self::open_window`].
    /// If the opener fails, `from` is switched back to its index
    /// (`Shown`: the reservation was removed, so nobody hosts it) before
    /// the queued `Park` is delivered; `from` then shows that workspace
    /// again and its `Park` arm ignores the event.
    pub fn tear_out(
        app: &Entity<ShellApp>,
        from: WindowKey,
        cx: &mut App,
    ) -> Result<WindowKey, OpenRefusal> {
        if app.read(cx).window_opener.is_none() {
            return Err(OpenRefusal::NoOpener);
        }
        let (old, key) = app.update(cx, |app, cx| {
            let workspaces = &app.services.workspaces;
            let Some(old) = workspaces.shown(from) else {
                return Err(OpenRefusal::NoFreeWorkspace);
            };
            let Some(next) = workspaces.lowest_free() else {
                return Err(OpenRefusal::NoFreeWorkspace);
            };
            match app.switch(from, next.get(), cx) {
                Switch::Shown | Switch::Moved { .. } => {}
                // `next` is shown by nobody and differs from `old`, and
                // nothing ran between the check and the switch.
                other => unreachable!("a switch to a free index landed as {other:?}"),
            }
            let key = app
                .reserve(old.get(), cx)
                .expect("the index this window just left is shown by nobody");
            Ok((old, key))
        })?;
        let spec = WindowSpec {
            workspace: old,
            placement: Placement::Beside(from),
        };
        let opener = app
            .read(cx)
            .window_opener
            .clone()
            .expect("checked before the reservation");
        let opened = opener(key, &spec, cx);
        app.update(cx, |app, cx| {
            let confirmed = app.confirm_open(key, opened.is_some(), cx);
            if confirmed.is_err() {
                let back = app.services.workspaces.switch(from, old.get());
                debug_assert_eq!(back, Switch::Shown, "the rolled-back index is unhosted");
            }
            confirmed
        })?;
        Ok(key)
    }

    /// The confirm step of an open: the opener opened a window and its
    /// view registered under `key`. Otherwise the reservation is undone
    /// (`remove_window`, and any registry entry the key left behind) and
    /// `OpenerFailed` is returned.
    fn confirm_open(
        &mut self,
        key: WindowKey,
        opened: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), OpenRefusal> {
        if opened && self.handle(key).is_some() {
            cx.emit(ShellEvent::WindowsChanged);
            return Ok(());
        }
        self.services.workspaces.remove_window(key);
        self.windows.retain(|(k, _)| *k != key);
        self.views.retain(|(k, _)| *k != key);
        Err(OpenRefusal::OpenerFailed)
    }

    /// Show index `n` in window `key` (`Workspaces::switch`), and start
    /// the park a `Moved` outcome needs: the move is already committed, so
    /// the window it came from is told to park the occupants it built for
    /// the index ([`ShellEvent::Park`]) and answers through
    /// [`Self::records_parked`]. Every other outcome parks nothing: a
    /// raise or a refusal changed no host, and a switch within one window
    /// keeps its occupants where they are.
    pub fn switch(&mut self, key: WindowKey, n: u8, cx: &mut Context<Self>) -> Switch {
        let outcome = self.services.workspaces.switch(key, n);
        if let Switch::Moved { from } = outcome {
            let workspace = WorkspaceIx::new(n).expect("a move names a valid index");
            cx.emit(ShellEvent::Park {
                window: from,
                workspace,
            });
        }
        outcome
    }

    /// A window's answer to `Park`: the records for `workspace` are in
    /// `parked_tiles`. Announced as [`ShellEvent::Parked`] for the window
    /// that now shows the index to pull them; see that variant for why
    /// this is a second event.
    pub fn records_parked(&mut self, workspace: WorkspaceIx, cx: &mut Context<Self>) {
        cx.emit(ShellEvent::Parked { workspace });
    }

    /// Bring window `key` to the front: a switch asked for an index that
    /// window shows (`Switch::Raise`). A key the registry does not list
    /// (closed, or never opened) is ignored.
    pub fn activate_window(&self, key: WindowKey, cx: &mut App) {
        if let Some(handle) = self.handle(key) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    }

    /// Record the window built for `key`, at the end of `ShellView::new`:
    /// its platform handle in the registry and its view, so the session
    /// writer can ask the view for its occupants' records. Every window
    /// registers here, the composition root's first window as much as one
    /// the opener builds, which is what makes [`Self::windows`] truthful.
    pub fn register_view(
        &mut self,
        key: WindowKey,
        view: WeakEntity<ShellView>,
        handle: AnyWindowHandle,
    ) {
        debug_assert!(
            self.handle(key).is_none(),
            "window {key:?} registered twice"
        );
        self.windows.push((key, handle));
        self.views.push((key, view));
        self.has_registered_view = true;
    }

    /// Every registered window's view that is still alive, in opening
    /// order. A window leaves this list through [`Self::window_closed`];
    /// a view that is gone without that (a window a fixture removed
    /// directly) is skipped.
    pub fn views(&self) -> impl Iterator<Item = Entity<ShellView>> + '_ {
        self.views.iter().filter_map(|(_, view)| view.upgrade())
    }

    /// Whether the watcher has work left: [`Self::any_window_open`]. A
    /// tick with no window would wake diagnostics and write a session for
    /// nobody.
    pub(super) fn watcher_runs(&self) -> bool {
        self.any_window_open()
    }

    /// Whether anything is, or is about to be, on screen: true until the
    /// first window registers, and then while the registry lists a
    /// window. The liveness every app-lifetime loop checks (the watcher,
    /// the bridge's drain and its retry lanes): each starts with this
    /// entity, ahead of its first window, and ends when the last window
    /// has deregistered. Neither `cx.windows()` nor a view entity's
    /// liveness decides, since the bridge's closures and the quit hooks
    /// hold a view past its window.
    pub fn any_window_open(&self) -> bool {
        !self.has_registered_view || !self.windows.is_empty()
    }

    /// Issue the next distinct-request tag: counts up from 1, never
    /// repeats within a run, and is shared by every window (see the
    /// field). An event, dispatch or constructor path only, as every app
    /// update is.
    pub fn next_tag(&mut self) -> u64 {
        self.next_tag += 1;
        self.next_tag
    }

    /// Where `delivery` goes — see [`Route`]. Keyless → every window; a
    /// shell-owned key (`is_shell_key`, `REFERENCE_KEY`, a watchlist key)
    /// → every window; otherwise the key is a tile id and the answer goes
    /// to the window hosting the tile's workspace (`Workspaces::
    /// window_of`), or nowhere while none does.
    pub fn route(&self, delivery: &Delivery) -> Route {
        self.route_key(delivery.key())
    }

    /// [`Self::route`] on a bare key: what a distinct answer, which
    /// carries no `Delivery` under a shell key, is routed by.
    pub fn route_key(&self, key: Option<QueryKey>) -> Route {
        let Some(key) = key else {
            return Route::Broadcast;
        };
        if super::is_shell_key(key) || key == super::REFERENCE_KEY || super::is_watchlist_key(key) {
            return Route::Broadcast;
        }
        match self.services.workspaces.window_of(TileId(key.0)) {
            Some(host) => Route::Window(host),
            None => Route::Drop,
        }
    }

    /// The windows `route` names, each as its platform handle and live
    /// view, in opening order: one for `Window`, every registered one for
    /// `Broadcast`, none for `Drop`. A window whose view is gone (a
    /// fixture that removed it without `window_closed`) is skipped.
    fn targets(&self, route: Route) -> Vec<(AnyWindowHandle, Entity<ShellView>)> {
        let pair = |key: WindowKey| {
            let handle = self.handle(key)?;
            let view = self
                .views
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, view)| view.upgrade())?;
            Some((handle, view))
        };
        match route {
            Route::Drop => Vec::new(),
            Route::Window(key) => pair(key).into_iter().collect(),
            Route::Broadcast => self.windows.iter().filter_map(|(k, _)| pair(*k)).collect(),
        }
    }

    /// Hand `delivery` to the window(s) its route names: inside each
    /// window's update, so the view has its `Window`, through
    /// `ShellView::deliver`. A `Broadcast` is cloned per window but the
    /// last; a `Window` route moves it; a `Drop` is logged at `trace` on
    /// `geode::query` and discarded, as is a route whose window is gone.
    ///
    /// An associated function, not a method: the route is resolved under
    /// one short read of the app and the lease is released before any
    /// window is updated, because a tile's `deliver` may reach back into
    /// the app (a flip barrier releasing, a session mark) and an update
    /// of a leased entity is refused.
    pub fn deliver(app: &Entity<ShellApp>, delivery: Delivery, cx: &mut App) {
        let (route, targets) = {
            let app = app.read(cx);
            let route = app.route(&delivery);
            (route, app.targets(route))
        };
        let Some((last, rest)) = targets.split_last() else {
            tracing::trace!(
                target: "geode::query",
                "dropped an answer for key {:?}: {}",
                delivery.key(),
                if route == Route::Drop { "no window hosts its tile" } else { "its window is gone" }
            );
            return;
        };
        for (handle, view) in rest {
            let copy = delivery.clone();
            let _ = handle.update(cx, |_, window, cx| {
                view.update(cx, |v, cx| v.deliver(copy, window, cx))
            });
        }
        let (handle, view) = last;
        let _ = handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| v.deliver(delivery, window, cx))
        });
    }

    /// Route a distinct answer under a shell-owned key (a picker's, an
    /// expression field's, an action value choice's) the way
    /// [`Self::deliver`] routes a `Delivery`: to every window, each of
    /// which applies it only to the surface holding the answer's tag
    /// (`ShellView::deliver_distinct`). A tile's key takes the
    /// `Delivery::Distinct` route instead. Same lease discipline as
    /// `deliver`.
    pub fn deliver_distinct(app: &Entity<ShellApp>, outcome: DistinctOutcome, cx: &mut App) {
        if !super::is_shell_key(outcome.key) {
            Self::deliver(app, Delivery::Distinct(outcome), cx);
            return;
        }
        let views: Vec<Entity<ShellView>> = app.read(cx).views().collect();
        let Some((last, rest)) = views.split_last() else {
            return;
        };
        for view in rest {
            let copy = outcome.clone();
            view.update(cx, |v, cx| v.deliver_distinct(copy, cx));
        }
        last.update(cx, |v, cx| v.deliver_distinct(outcome, cx));
    }

    /// Show a position command's answer as every window's status notice
    /// (`ShellView::note_command`): the command was sent from one window,
    /// but nothing names it, and the answer is informational. Same lease
    /// discipline as `deliver`.
    pub fn note_command(app: &Entity<ShellApp>, outcome: &CommandOutcome, cx: &mut App) {
        let views: Vec<Entity<ShellView>> = app.read(cx).views().collect();
        for view in views {
            view.update(cx, |v, cx| v.note_command(outcome, cx));
        }
    }

    /// The open windows, in opening order: each key and its platform
    /// handle.
    pub fn windows(&self) -> &[(WindowKey, AnyWindowHandle)] {
        &self.windows
    }

    /// The platform handle of window `key`; `None` once it closed.
    pub fn handle(&self, key: WindowKey) -> Option<AnyWindowHandle> {
        self.windows
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, handle)| *handle)
    }

    /// Whether no window other than `key` is registered: the close that
    /// quits, rather than one that parks.
    pub fn is_last_window(&self, key: WindowKey) -> bool {
        !self.windows.iter().any(|(k, _)| *k != key)
    }

    /// Window `key` is closing: the close path calls this while the
    /// window still stands (inside should-close), so its view is alive to
    /// hear the parks. Every index the window hosted leaves `Workspaces`
    /// (`remove_window`) and is parked through the ordinary
    /// [`ShellEvent::Park`], so a tile in a workspace the window showed
    /// or hid keeps its record in `parked_tiles` for the next window that
    /// shows the index, or for the session file. The key then leaves the
    /// registry and the view list, and `WindowsChanged` tells every
    /// window the count moved.
    pub fn window_closed(&mut self, key: WindowKey, cx: &mut Context<Self>) {
        for workspace in self.services.workspaces.remove_window(key) {
            cx.emit(ShellEvent::Park {
                window: key,
                workspace,
            });
        }
        self.windows.retain(|(k, _)| *k != key);
        self.views.retain(|(k, _)| *k != key);
        cx.emit(ShellEvent::WindowsChanged);
    }

    /// The title of window `key`: `Geode` while it is the only registered
    /// window, `Geode · N` (N the index it shows) once there are two or
    /// more, so each window on a screen says which workspace it is. A
    /// pure read, for the event paths that set the title; never from
    /// render.
    pub fn window_title(&self, key: WindowKey) -> String {
        let shown = self.services.workspaces.shown(key);
        match shown {
            Some(ix) if self.windows.len() >= 2 => format!("Geode · {}", ix.get()),
            _ => "Geode".to_string(),
        }
    }

    /// Hand window `key` the parked records it should host, removing them
    /// from here: a record whose tile sits in a workspace `key` hosts
    /// (`Workspaces::window_of`), and, for the oldest open window only, a
    /// record whose tile is in no workspace at all, so the first window
    /// owns unplaced records and writes them back as it always has.
    /// Everything else stays parked for the window that hosts it. A record
    /// that leaves is the window's to write back
    /// (`ShellView::current_tiles`), through its occupant or its own
    /// unplaced-record map. With one window open, every record is its.
    pub fn take_parked_records(&mut self, key: WindowKey) -> TileRecords {
        let workspaces = &self.services.workspaces;
        let oldest = workspaces.windows().next().map(|(k, _)| k);
        let (taken, kept): (TileRecords, TileRecords) = std::mem::take(&mut self.parked_tiles)
            .into_iter()
            .partition(|(id, _)| match workspaces.window_of(TileId(*id)) {
                Some(host) => host == key,
                None => oldest == Some(key),
            });
        self.parked_tiles = kept;
        taken
    }

    /// A layout change awaits the next periodic snapshot. Called from a
    /// window's event, dispatch and constructor paths, never from its
    /// draw pass (`ShellView::mark_session_dirty`).
    pub fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    pub fn palette_usage(&self) -> &PaletteUsage {
        &self.palette_usage
    }

    /// A palette row was chosen: count the use at `now` (unix seconds)
    /// and move the usage version on, so the flush sees the change even
    /// when the row mutated nothing else.
    pub fn note_palette_use(&mut self, key: &str, now: u64) {
        self.palette_usage.record(key, now);
        self.palette_usage_version += 1;
    }

    /// The tile records every save writes: each live window's own
    /// (`ShellView::current_tiles`: its non-placeholder occupants, then
    /// its unplaced records), then the records still parked here. An
    /// earlier source wins an id: a window's live occupant over a stale
    /// parked record for the same tile.
    pub(super) fn current_tiles(&self, cx: &App) -> TileRecords {
        let mut tiles = TileRecords::new();
        // Every registered view: a window leaves the registry through
        // `window_closed`, which parked its tiles first, so a view still
        // listed here holds live records and one that left has its records
        // in `parked_tiles`.
        for view in self.views() {
            for (id, record) in view.read(cx).current_tiles(cx) {
                tiles.entry(id).or_insert(record);
            }
        }
        for (id, record) in &self.parked_tiles {
            tiles.entry(*id).or_insert_with(|| record.clone());
        }
        tiles
    }

    /// The page records every save writes: each live window's
    /// (`ShellView::current_pages`), a later window winning a kind both
    /// retain.
    pub(super) fn current_pages(&self, cx: &App) -> PageRecords {
        let mut pages = PageRecords::new();
        for view in self.views() {
            pages.extend(view.read(cx).current_pages(cx));
        }
        pages
    }

    pub fn services(&self) -> &ShellServices {
        &self.services
    }

    pub fn config(&self) -> &Config {
        &self.services.config
    }

    pub fn frame(&self) -> &Entity<Frame> {
        &self.frame
    }

    pub fn diagnostics(&self) -> &Entity<Diagnostics> {
        &self.diagnostics
    }

    /// Bumped by every applied reload; config dialog rows are keyed by it.
    pub fn config_revision(&self) -> u64 {
        self.config_revision
    }

    /// The outcome of the last reload attempt, `Unchanged` before the first.
    pub fn last_reload(&self) -> &reload::ReloadOutcome {
        &self.last_reload
    }

    /// The status bar's rejected-reload segment, prepared when the last
    /// reload was refused; `None` otherwise.
    pub fn reload_status(&self) -> Option<&SharedString> {
        self.reload_status.as_ref()
    }

    /// The status bar's restart notice while restart-sensitive config
    /// differs from what the data engine started with.
    pub fn restart_required(&self) -> Option<&SharedString> {
        self.restart_required.as_ref()
    }

    pub fn font_size(&self) -> FontSize {
        self.font_size
    }

    pub fn find_style(&self) -> FindStyle {
        self.find_style
    }

    pub fn add_direction(&self) -> AddDirection {
        self.add_direction
    }

    pub fn line_numbers(&self) -> LineNumbers {
        self.line_numbers
    }

    pub fn default_source(&self) -> Option<&str> {
        self.default_source.as_deref()
    }

    pub fn link_include_tile_filter(&self) -> bool {
        self.link_include_tile_filter
    }

    /// Today on the configured clock, as of the watcher's last tick.
    pub fn today(&self) -> chrono::NaiveDate {
        self.today
    }

    /// Apply a loaded candidate exactly as the file watcher would —
    /// cross-crate test reach: `geode-app`'s composition tests reload
    /// without a watcher (`apply_reload` is `pub(super)`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn apply_reload_for_test(&mut self, config: Config, cx: &mut Context<Self>) {
        self.apply_reload(config, cx);
    }
}
