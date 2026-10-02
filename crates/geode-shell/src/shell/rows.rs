//! The shell's refresh seam for config dialog rows. Each open dialog's prepared
//! list is re-keyed here; the call sites are the events that can change a key
//! input: open, a dialog key, the dialog input's Change, a dialog pointer
//! transition, an applied reload, and a covered object dialog's reveal. Cheap when nothing changed: one key
//! comparison per open dialog.

use gpui::App;

use super::ShellView;

impl ShellView {
    /// Refresh every open dialog's prepared rows against its current inputs,
    /// covered dialogs included, except a parked object dialog (see below).
    pub(crate) fn refresh_dialog_rows(&mut self, _cx: &App) {
        let revision = self.config_revision;
        if let Some(state) = self.keybindings.as_mut() {
            state.refresh_rows(&self.services.registry, &self.services.keymap, revision);
        }
        // The live object dialog only: a parked one is re-keyed when it is revealed
        // (`close_modal`), since nothing reads it while it is covered.
        if let Some(state) = self.object_dialog.as_mut() {
            state.refresh_rows(&self.services.config, revision);
        }
    }

    /// Refuse a stale prepared list. Called from each config dialog's `build`;
    /// a missed refresh fails a test or a debug run here instead of painting rows
    /// the handlers no longer agree with.
    #[cfg(debug_assertions)]
    pub(crate) fn assert_rows_current(&self, _cx: &App) {
        if let Some(state) = self.keybindings.as_ref() {
            assert!(
                state.rows.is_current(&self.config_revision, &state.query),
                "prepared rows are stale: keybindings changed an input without refresh_dialog_rows"
            );
        }
        if let Some(state) = self.object_dialog.as_ref() {
            assert!(
                state.rows.is_current(&self.config_revision, &state.query),
                "prepared rows are stale: the object dialog changed an input without refresh_dialog_rows"
            );
        }
    }
}
