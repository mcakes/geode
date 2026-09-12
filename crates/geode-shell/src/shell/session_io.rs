//! Session layout persistence (spec section 3.6, session.toml): the
//! coalesced dirty-write extractor `new`'s background flush loop drains,
//! and the synchronous save path a few dispatched actions call directly
//! (workspace switches, tile close). Split out of `shell/mod.rs`
//! (Phase 3c Task 0) as the one seam that touches the filesystem outside
//! `geode-data`.

use gpui::App;
use std::path::PathBuf;

use crate::session::{self, FrameRecord};

use super::ShellView;

impl ShellView {
    /// Build the `[frame]` record `to_toml`/`save` write from the frame's
    /// current state (Phase 4a §3.6) — shared by the flush path below and
    /// `save_session`'s synchronous one-shot.
    fn frame_record(&self, cx: &App) -> FrameRecord {
        let frame = self.frame.read(cx);
        FrameRecord {
            scope: frame.scope().clone(),
            active_slot: frame.active_slot(),
            as_of: frame.as_of().clone(),
        }
    }

    /// If a workspace mutation happened since the last flush, a live
    /// occupant's serialized state differs from what was last written
    /// (Task 4 — a state-only change never sets `session_dirty`, since
    /// that flag tracks the layout only), or the frame's own restorable
    /// state (scope, active slot, as-of — Phase 4a §3.6) changed since the
    /// last flush, serialize the current session state (cheap:
    /// `session::to_string_pretty` over a handful of small TOML tables —
    /// safe to run synchronously here, on the UI thread, unlike the actual
    /// file write) and clear the dirty flag, handing the caller `(path,
    /// text)` to write off the UI thread. Returns `None` when there's
    /// nothing to flush (nothing dirty, no session path configured, or
    /// serialization somehow failed — logged as a warning either way,
    /// never a panic).
    ///
    /// Called from the background watcher's ~500ms tick (`new`) in
    /// production. Tests call it directly instead of driving that timer:
    /// gpui's test executor never advances its simulated clock under
    /// `run_until_parked` (same reasoning as `apply_reload`'s doc comment),
    /// so there's no practical way to wait out a real ~500ms poll in a
    /// `#[gpui::test]` — this is the real flush logic either way; the
    /// watcher loop is just what schedules calling it.
    pub(super) fn take_dirty_session_write(&mut self, cx: &App) -> Option<(PathBuf, String)> {
        let tiles = self.current_tiles(cx);
        let versions = self.frame.read(cx).versions();
        let frame_versions = (versions.scope, versions.grouping, versions.as_of);
        let frame_dirty = frame_versions != self.last_frame_versions_written;
        let usage_dirty = self.palette_usage_version != self.last_palette_usage_written;
        // A module state change never sets `session_dirty` (that flag
        // tracks the layout only), so this compares the freshly-gathered
        // tiles (and the frame's own restorable versions) against what was
        // last written — either kind of change is still noticed here, on
        // the same tick, without dirtying the layout flag on every
        // keystroke inside a module or every scope edit.
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

    /// Write the current workspace layout and every occupant's tile record
    /// (Task 4) to the session file, if one is configured
    /// (`ShellServices::session_path`), synchronously and unconditionally
    /// (ignores `session_dirty` — this is the "flush no matter what" path,
    /// not the coalesced per-dispatch one). The only caller is `main.rs`'s
    /// best-effort `on_app_quit` hook: a one-shot at shutdown, not a
    /// per-keystroke hot path, so a synchronous atomic write
    /// (`session::save`) here is fine — it does not reintroduce the
    /// render-thread stall Task 3 fix round 1 removed from `dispatch`. A
    /// write failure (e.g. an unwritable directory) is a warning line,
    /// never a panic — session persistence is a convenience, not a
    /// correctness requirement (mirrors config's own "bad input is a
    /// warning" philosophy).
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
