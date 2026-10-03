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
    pub(crate) fn refresh_dialog_rows(&mut self, cx: &App) {
        let revision = self.config_revision;
        if let Some(state) = self.keybindings.as_mut() {
            state.refresh_rows(&self.services.registry, &self.services.keymap, revision);
        }
        // The live object dialog only: a parked one is re-keyed when it is revealed
        // (`close_modal`), since nothing reads it while it is covered.
        let Some(domain) = self.object_dialog.as_ref().map(|state| state.domain) else {
            return;
        };
        let (generation, lead) = self.object_rows_inputs(domain, cx);
        if let Some(state) = self.object_dialog.as_mut() {
            let key = state.rows_key(revision, generation);
            state.refresh_rows(&self.services.config, key, lead);
        }
    }

    /// What an object dialog's rows read from the frame: the generation
    /// their key carries and the leading rows. Zero and none for a domain
    /// whose rows are the configuration alone. Never call this from inside a
    /// `frame.update`: it reads the frame entity.
    fn object_rows_inputs(
        &self,
        domain: super::objectdialog::Domain,
        cx: &App,
    ) -> (u64, Vec<super::objectdialog::ObjectRow>) {
        if !domain.applies_from_browse() {
            return (0, Vec::new());
        }
        let frame = self.target_frame();
        let generation = frame.entity().read(cx).generation();
        (generation, domain.lead_rows(&frame.read(cx)))
    }

    /// Refuse a stale prepared list. Called from each config dialog's `build`;
    /// a missed refresh fails a test or a debug run here instead of painting rows
    /// the handlers no longer agree with.
    #[cfg(debug_assertions)]
    pub(crate) fn assert_rows_current(&self, cx: &App) {
        if let Some(state) = self.keybindings.as_ref() {
            assert!(
                state.rows.is_current(&self.config_revision, &state.query),
                "prepared rows are stale: keybindings changed an input without refresh_dialog_rows"
            );
        }
        if let Some(state) = self.object_dialog.as_ref() {
            let (generation, _) = self.object_rows_inputs(state.domain, cx);
            assert!(
                state.rows.is_current(
                    &state.rows_key(self.config_revision, generation),
                    &state.query
                ),
                "prepared rows are stale: the object dialog changed an input without refresh_dialog_rows"
            );
        }
    }
}
