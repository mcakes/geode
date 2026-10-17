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

use std::path::PathBuf;

use gpui::{App, AppContext, Context, Entity, WeakEntity};

use super::{ShellServices, ShellView, hot_reload};
use crate::diagnostics::Diagnostics;
use crate::frame::Frame;
use crate::palette_usage::PaletteUsage;
use crate::session::{PageRecords, TileRecords};
use crate::tiling::WindowKey;
use geode_core::config::Config;

pub struct ShellApp {
    pub(crate) services: ShellServices,
    /// Config directories read by each watcher poll; `None` layers are
    /// skipped by the scan and every persist path is a silent no-op
    /// without a user directory.
    pub(crate) desk_dir: Option<PathBuf>,
    pub(crate) user_dir: Option<PathBuf>,
    /// The shared frame: one set of lanes, link groups and version counters
    /// for every window and every occupant.
    frame: Entity<Frame>,
    /// Shared diagnostics state, fed by the app bridge and config
    /// load/reload. Each window observes it for the requests its surfaces
    /// drain.
    diagnostics: Entity<Diagnostics>,
    /// The last key issued by [`Self::register_window`]; keys count up
    /// from 1 and are never reused.
    next_window: u64,
    /// Every window's view, registered at the end of `ShellView::new`.
    /// Weak, so this owner never keeps a closed window's view alive;
    /// [`Self::views`] skips a handle whose view is gone and
    /// [`Self::register_view`] drops such handles as it adds the next.
    pub(super) views: Vec<(WindowKey, WeakEntity<ShellView>)>,
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
        // `ShellView::apply_reload` so the section is consistent before and
        // after reload. The clock itself is a window's global
        // (`ShellView::new` sets `AppClock`); only its diagnostics are
        // read here.
        let startup_diagnostics = {
            let cfg = &services.config;
            let mut diags = cfg.diagnostics.clone();
            diags.extend(crate::defaults::mod_alias_from_config(cfg).1);
            diags.extend(crate::defaults::modules_default_diagnostic(cfg));
            // Warn when the default timeseries source names no fetch source.
            diags.extend(crate::series::default_source_diagnostic(cfg));
            diags.extend(geode_core::clock::Clock::from_config(cfg).1);
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

        Self {
            services,
            desk_dir,
            user_dir,
            frame,
            diagnostics,
            next_window: 0,
            views: Vec::new(),
            parked_tiles,
            palette_usage,
            palette_usage_version: 0,
            last_palette_usage_written: 0,
            session_dirty: false,
            last_tiles_written: TileRecords::new(),
            last_pages_written: PageRecords::new(),
            last_frame_generation_written: 0,
            last_session_text: None,
        }
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
    }

    /// Every window's view that is still alive, in registration order. A
    /// closed window's handle no longer upgrades and is skipped.
    pub fn views(&self) -> impl Iterator<Item = Entity<ShellView>> + '_ {
        self.views.iter().filter_map(|(_, view)| view.upgrade())
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
}
