//! The app-level owner of [`ShellServices`]: one entity every window's
//! [`ShellView`](super::ShellView) reads through its [`WindowKey`].
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

use std::path::PathBuf;

use gpui::{AppContext, Context, Entity};

use super::{ShellServices, hot_reload};
use crate::diagnostics::Diagnostics;
use crate::frame::Frame;
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

        Self {
            services,
            desk_dir,
            user_dir,
            frame,
            diagnostics,
            next_window: 0,
        }
    }

    /// Issue the next window key. Called once per window, before its
    /// `ShellView` is built.
    pub fn register_window(&mut self) -> WindowKey {
        self.next_window += 1;
        WindowKey(self.next_window)
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
