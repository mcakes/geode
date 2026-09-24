//! Session snapshots for periodic background writes and synchronous shutdown.
//! Snapshot comparison happens on the UI thread. A returned snapshot is marked
//! handled before I/O, so these baselines do not acknowledge a successful save.

use gpui::App;
use std::path::PathBuf;

use crate::session::{self, FrameRecord};

use super::ShellView;

impl ShellView {
    /// Capture the frame fields shared by periodic and shutdown saves.
    fn frame_record(&self, cx: &App) -> FrameRecord {
        let frame = self.frame.read(cx);
        FrameRecord {
            scope: frame.scope().clone(),
            active_slot: frame.active_slot(),
            as_of: frame.as_of().clone(),
        }
    }

    /// Extract a snapshot when layout dirt, serialized tile state, frame
    /// versions, or palette usage differ from the last extracted snapshot.
    /// Called by the watcher's periodic tick; this method performs no file I/O.
    ///
    /// The layout flag is cleared before checking the path or serializing.
    /// On successful serialization, comparison baselines advance before the
    /// caller writes the file. A disk failure therefore does not retry unchanged
    /// state on the next tick. Serialization failure is logged without advancing
    /// those baselines, but a layout-only change can still lose its dirty flag.
    /// No configured path or no detected change returns `None` silently.
    pub(super) fn take_dirty_session_write(&mut self, cx: &App) -> Option<(PathBuf, String)> {
        let tiles = self.current_tiles(cx);
        let versions = self.frame.read(cx).versions();
        let frame_versions = (versions.scope, versions.grouping, versions.as_of);
        let frame_dirty = frame_versions != self.last_frame_versions_written;
        let usage_dirty = self.palette_usage_version != self.last_palette_usage_written;
        // Module state changes do not set the layout flag. Compare serialized
        // records and frame/usage versions to catch independent changes.
        if !self.session_dirty && !frame_dirty && !usage_dirty && tiles == self.last_tiles_written {
            return None;
        }
        self.session_dirty = false;
        let path = self.services.session_path.clone()?;
        let record = self.frame_record(cx);
        match session::to_string_pretty(
            &self.services.workspaces,
            &tiles,
            Some(&record),
            &self.palette_usage,
        ) {
            Ok(text) => {
                self.last_tiles_written = tiles;
                self.last_frame_versions_written = frame_versions;
                self.last_palette_usage_written = self.palette_usage_version;
                Some((path, text))
            }
            Err(e) => {
                tracing::warn!(target: "geode::session", "failed to serialize session: {e}");
                None
            }
        }
    }

    /// Synchronously save current state when a session path is configured,
    /// regardless of the dirty flag or periodic-save baselines. The app's quit
    /// hook calls this as a final best-effort save; failures are logged.
    /// It does not wait for an in-flight periodic write, which may rename last.
    pub fn save_session(&self, cx: &App) {
        let Some(path) = self.services.session_path.as_ref() else {
            return;
        };
        let record = self.frame_record(cx);
        if let Err(e) = session::save(
            path,
            &self.services.workspaces,
            &self.current_tiles(cx),
            Some(&record),
            &self.palette_usage,
        ) {
            tracing::warn!(target: "geode::session", "failed to save session: {e}");
        }
    }
}
