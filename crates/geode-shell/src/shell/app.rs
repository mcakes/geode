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
use std::sync::Arc;

use gpui::{App, AppContext, Context, Entity, EventEmitter, SharedString, WeakEntity};

use super::{ShellEvent, ShellServices, ShellView, hot_reload};
use crate::diagnostics::Diagnostics;
use crate::fontsize::FontSize;
use crate::frame::Frame;
use crate::linenumbers::LineNumbers;
use crate::palette_usage::PaletteUsage;
use crate::reload;
use crate::session::{PageRecords, TileRecords};
use crate::tileadd::AddDirection;
use crate::tiling::WindowKey;
use crate::vimfind::FindStyle;
use geode_core::config::{Config, LayerDoc};

impl EventEmitter<ShellEvent> for ShellApp {}

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
    /// The last key issued by [`Self::register_window`]; keys count up
    /// from 1 and are never reused.
    next_window: u64,
    /// Every window's view, registered at the end of `ShellView::new`.
    /// Weak, so this owner never keeps a closed window's view alive;
    /// [`Self::views`] skips a handle whose view is gone and
    /// [`Self::register_view`] drops such handles as it adds the next.
    pub(super) views: Vec<(WindowKey, WeakEntity<ShellView>)>,
    /// Whether any view has registered yet. The watcher ends once every
    /// registered view is gone, but not before the first registers: the
    /// loop starts with this entity, ahead of its first window.
    has_registered_view: bool,
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
    /// Session tile records no window has taken yet, seeded from the
    /// restored session. A window drains them at construction through
    /// [`Self::take_parked_records`]; whatever remains here is written
    /// back by every save, after the live records, so a tile no window
    /// hosts keeps its module and state across a restart.
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
            views: Vec::new(),
            has_registered_view: false,
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

    /// Issue the next window key. Called once per window, before its
    /// `ShellView` is built.
    pub fn register_window(&mut self) -> WindowKey {
        self.next_window += 1;
        WindowKey(self.next_window)
    }

    /// Record the view built for `key`, at the end of `ShellView::new`, so
    /// the session writer can ask it for its occupants' records. Handles
    /// whose view has gone are dropped here, as the next window registers.
    pub fn register_view(&mut self, key: WindowKey, view: WeakEntity<ShellView>) {
        self.views.retain(|(_, v)| v.upgrade().is_some());
        self.views.push((key, view));
        self.has_registered_view = true;
    }

    /// Every window's view that is still alive, in registration order. A
    /// closed window's handle no longer upgrades and is skipped.
    pub fn views(&self) -> impl Iterator<Item = Entity<ShellView>> + '_ {
        self.views.iter().filter_map(|(_, view)| view.upgrade())
    }

    /// The views whose window is still open, in registration order. The
    /// window decides, not the entity: the bridge's closures and the quit
    /// hooks hold a view past its window. What the watcher gathers for
    /// the diagnostics page and whether it ticks at all read this; the
    /// session union reads [`Self::views`] (see `current_tiles`).
    pub(super) fn open_views<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = Entity<ShellView>> + 'a {
        let open = cx.windows();
        self.views()
            .filter(move |view| open.contains(&view.read(cx).window_handle))
    }

    /// Whether the watcher has work left: until the first view registers,
    /// and then while any registered view's window is still open. A tick
    /// for a closed window would wake its diagnostics and write its
    /// session for nobody.
    pub(super) fn watcher_runs(&self, cx: &App) -> bool {
        !self.has_registered_view || self.open_views(cx).next().is_some()
    }

    /// Hand a window the parked records it should host, removing them
    /// from here. While there is one window every record goes to it; this
    /// is the seam a later window-to-workspace mapping narrows to the
    /// tiles `_key`'s window hosts. A record that leaves is the window's
    /// to write back (`ShellView::current_tiles`), through its occupant
    /// or its own unplaced-record map.
    pub fn take_parked_records(&mut self, _key: WindowKey) -> TileRecords {
        std::mem::take(&mut self.parked_tiles)
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
        // Deliberately every registered view, open window or not: until the
        // close path parks a closed window's tiles and deregisters its view,
        // skipping an entity-alive closed view would drop its tiles from the
        // file.
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
