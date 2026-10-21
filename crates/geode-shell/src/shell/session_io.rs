//! Session snapshots for periodic background writes and synchronous shutdown,
//! made by the app-level owner: one writer for every window, over the union
//! of each live window's records and the records no window has taken, plus
//! one placement record per registered window, read from the live windows
//! before the text is built.
//! Snapshot comparison happens on the UI thread. A returned snapshot is marked
//! handled before I/O, so these baselines do not acknowledge a successful save.

use gpui::{App, Context, Entity, Window};
use std::path::PathBuf;

use crate::frame::GroupingChoice;
use crate::session::{self, FrameRecord, WindowRecord};
use crate::tiling::WindowKey;
use geode_core::link::Group;

use super::ShellApp;

impl ShellApp {
    /// Capture the shared lane's `[frame]` record for periodic and shutdown
    /// saves. Pinned lanes are workspace-owned and written by `pinned_records`.
    fn frame_record(&self, cx: &App) -> FrameRecord {
        let frame = self.frame().read(cx).shared();
        FrameRecord {
            scope: frame.scope().clone(),
            active_slot: frame.active_slot(),
            ad_hoc: frame.ad_hoc().map(<[String]>::to_vec),
            ad_hoc_active: frame.grouping_choice() == GroupingChoice::AdHoc,
            loaded_from: frame.loaded_from().map(str::to_string),
            as_of: frame.as_of().clone(),
        }
    }

    /// Capture each pinned workspace's own lane for `workspaces.N.frame`.
    /// Pin, unpin, and lane edits advance the frame generation, so the
    /// periodic dirty check already covers changes here.
    fn pinned_records(&self, cx: &App) -> session::PinnedRecords {
        let frame = self.frame().read(cx);
        frame
            .pinned_workspaces()
            .map(|ws| {
                let lane = frame.view(ws);
                (
                    ws,
                    FrameRecord {
                        scope: lane.scope().clone(),
                        active_slot: lane.active_slot(),
                        ad_hoc: lane.ad_hoc().map(<[String]>::to_vec),
                        ad_hoc_active: lane.grouping_choice() == GroupingChoice::AdHoc,
                        loaded_from: lane.loaded_from().map(str::to_string),
                        as_of: lane.as_of().clone(),
                    },
                )
            })
            .collect()
    }

    /// Each link group's scope, in `Group::ALL` order, for `[links]`.
    fn group_scopes(&self, cx: &App) -> session::GroupScopes {
        let frame = self.frame().read(cx);
        Group::ALL.map(|group| frame.group_scope(group).clone())
    }

    /// Extract a snapshot when layout dirt, serialized tile or page state,
    /// the frame generation, or palette usage differ from the last extracted
    /// snapshot.
    /// Called by the watcher's periodic tick; this method performs no file I/O.
    ///
    /// The layout flag is cleared before checking the path or serializing.
    /// On successful serialization, comparison baselines advance before the
    /// caller writes the file. A disk failure therefore does not retry unchanged
    /// state on the next tick. Serialization failure is logged without advancing
    /// those baselines, but a layout-only change can still lose its dirty flag.
    /// No configured path or no detected change returns `None` silently, and
    /// so does a snapshot that differs from the last one returned only in a
    /// link group's scope. The comparison is made on the text without the
    /// `[links]` tables, and `last_session_text` holds that links-free
    /// text: a group's scope advances the frame generation with every move
    /// of an emitting tile's cursor, and alone must not rewrite the file.
    /// The text returned for the file does carry the groups' scopes, so
    /// whenever a snapshot is written for another reason they go with it.
    ///
    /// The records are read from every live window (`current_tiles`,
    /// `current_pages`); a handle whose window has closed is dropped here.
    /// The window records (`window_records`) are read from the live
    /// windows once a write is due, never inside a window's update: the
    /// watcher runs from this entity's, so no window is `current`. A move
    /// or resize marks the session dirty through the view's bounds
    /// observer (`observe_window_bounds`), so the new bounds reach the
    /// file with the next periodic snapshot, or the final save.
    pub(crate) fn take_dirty_session_write(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(PathBuf, String)> {
        self.views.retain(|(_, v)| v.upgrade().is_some());
        let tiles = self.current_tiles(cx);
        let pages = self.current_pages(cx);
        let frame_generation = self.frame().read(cx).generation();
        let frame_dirty = frame_generation != self.last_frame_generation_written;
        let usage_dirty = self.palette_usage_version != self.last_palette_usage_written;
        // Module and page state changes do not set the layout flag. Compare
        // serialized records and frame/usage versions to catch independent
        // changes.
        if !self.session_dirty
            && !frame_dirty
            && !usage_dirty
            && tiles == self.last_tiles_written
            && pages == self.last_pages_written
        {
            return None;
        }
        self.session_dirty = false;
        let path = self.services.session_path.clone()?;
        let windows = self.window_records(None, cx);
        let record = self.frame_record(cx);
        let mut table = session::to_toml(
            &self.services.workspaces,
            &tiles,
            Some(&record),
            &self.pinned_records(cx),
            &self.palette_usage,
            &pages,
            &windows,
        );
        let serialized = session::table_to_string(&table).and_then(|bare| {
            // Compared before the groups' scopes are added: see the method's
            // doc. Unchanged, the snapshot is not written and nothing more
            // is serialized.
            if self.last_session_text.as_deref() == Some(bare.as_str()) {
                return Ok(None);
            }
            let text = if session::insert_links(&mut table, &self.group_scopes(cx)) {
                session::table_to_string(&table)?
            } else {
                bare.clone()
            };
            Ok(Some((bare, text)))
        });
        match serialized {
            Ok(snapshot) => {
                self.last_tiles_written = tiles;
                self.last_pages_written = pages;
                self.last_frame_generation_written = frame_generation;
                self.last_palette_usage_written = self.palette_usage_version;
                let (bare, text) = snapshot?;
                self.last_session_text = Some(bare);
                Some((path, text))
            }
            Err(e) => {
                tracing::warn!(target: "geode::session", "failed to serialize session: {e}");
                None
            }
        }
    }

    /// Synchronously save current state when a session path is configured,
    /// regardless of the dirty flag or periodic-save baselines, the link
    /// groups' scopes always included. The app's window should-close
    /// hook and its quit hook call this as a final best-effort save;
    /// failures are logged.
    /// It does not wait for an in-flight periodic write, which may rename last.
    ///
    /// An associated function, not a method: the window records are read
    /// from the live windows (`window_records`), which updates each window
    /// and so needs `&mut App`, which no read lease on the app can share.
    /// The records are collected first, under a short update of the app
    /// with nothing else inside; the app is then read once to build the
    /// text and write it. `current` is the window whose update this runs
    /// inside: the should-close hook passes its own, which cannot be
    /// updated again from within; the quit hook passes `None`, as it runs
    /// in no window's update.
    pub fn save_session(
        app: &Entity<ShellApp>,
        current: Option<(WindowKey, &Window)>,
        cx: &mut App,
    ) {
        // With no session path nothing will be written, so no window is
        // touched for it.
        if app.read(cx).services.session_path.is_none() {
            return;
        }
        let windows = app.update(cx, |a, cx| a.window_records(current, cx));
        app.read(cx).write_session(&windows, cx);
    }

    /// The write behind [`Self::save_session`]: the guard, then one
    /// synchronous [`session::save`] over this app's records and
    /// `windows`.
    ///
    /// While no window is registered nothing is written: a closed
    /// window's occupants are gone with it, and a snapshot taken then
    /// would replace their records with placeholders. The should-close
    /// hook saved while the window still stood; the quit hook that follows
    /// a close finds the registry empty and leaves that save alone, as an
    /// explicit quit's windows are still registered when its hook runs.
    fn write_session(&self, windows: &[WindowRecord], cx: &App) {
        let Some(path) = self.services.session_path.as_ref() else {
            return;
        };
        if self.windows().is_empty() {
            tracing::debug!(target: "geode::session", "no window registered; the session is not saved");
            return;
        }
        let record = self.frame_record(cx);
        if let Err(e) = session::save(
            path,
            &self.services.workspaces,
            &self.current_tiles(cx),
            Some(&record),
            &self.pinned_records(cx),
            &self.group_scopes(cx),
            &self.palette_usage,
            &self.current_pages(cx),
            windows,
        ) {
            tracing::warn!(target: "geode::session", "failed to save session: {e}");
        }
    }
}
