//! Palette lifecycle, selection scrolling, and action/theme/scope dispatch.
//! Opening snapshots items and usage; closing restores the preceding focus target.
//! The Input owns query text. Unconsumed keys reach palette navigation and commit
//! handling without entering the ordinary shell matcher.

use gpui::{
    Context, Focusable as _, KeyDownEvent, ScrollStrategy, UniformListScrollHandle, Window,
};

use crate::listfilter;
use crate::palette::{self, PaletteItem, PaletteState};

use super::ShellView;
use super::keys::convert_keystroke;

impl ShellView {
    /// Cancel any open command prompt, then toggle the palette. Opening also
    /// closes the stack list, clears pending key sequences, snapshots items/usage,
    /// resets Input and scroll state, and focuses the query field. Remember whether
    /// focus came from the toolbar filter so closing can restore it.
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_command_line(window, cx);
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        // Palette toggles are recognized ahead of command-line key ownership;
        // close that prompt first so only the new surface owns input.
        self.close_stack_list(cx);
        self.close_add_filter_menu(cx);
        self.close_row_menu(cx);
        // Pending matcher state cannot survive an overlay with separate key
        // routing, or later shell keys could complete an abandoned sequence.
        self.matcher.cancel();
        let bindings = palette::build_binding_index(&self.services.keymap);
        let saved = self.frame.read(cx).saved_scopes().clone();
        let items = palette::build_items(
            &self.services.registry,
            &self.services.theme,
            &bindings,
            &saved,
        );
        self.palette = Some(PaletteState::with_usage(
            items,
            &self.palette_usage,
            unix_now(),
        ));
        // Fresh scroll state for a fresh palette session — a stale offset
        // left over from a previous open (a different query, a different
        // scroll position) must not carry over now that the results list
        // scrolls a real viewport instead of always fitting on screen.
        self.palette_scroll = UniformListScrollHandle::new();
        self.palette_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Recorded before the palette takes focus, for `close_palette`
        // (see `ShellView::overlay_return_to_filter`). The flag belongs to the
        // stack's base when dialogs are open: a palette over them returns to the
        // top dialog, not to the field.
        if !self.modal_open() {
            self.overlay_return_to_filter = self.filter_field_focused(window, cx);
        }
        self.palette_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    /// Close an open palette and restore focus through the overlay-return path,
    /// or, over an open dialog stack, to the top dialog. A closed palette is a
    /// no-op. Reload closes without a Window through its separate deferred
    /// focus-restoration path.
    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Do not move focus when this method is called with no palette open.
        if self.palette.take().is_none() {
            return;
        }
        if self.modal_open() {
            super::dialog::refocus_top(self, window, cx);
        } else {
            self.return_focus_from_overlay(window, cx);
        }
    }

    /// Scroll the selected result into view after query or selection changes,
    /// moving the list only as far as the nearest edge; no-op when closed.
    pub(super) fn sync_palette_scroll(&self) {
        if let Some(palette) = self.palette.as_ref() {
            self.palette_scroll
                .scroll_to_item(palette.selected(), ScrollStrategy::Nearest);
        }
    }

    /// Record and dispatch a chosen row after the palette has closed. Actions
    /// use normal dispatch, themes apply/persist directly, and Scope rows load the
    /// current saved scope. A palette-toggle row only closes and is not recorded:
    /// dispatching it again would reopen the palette.
    fn dispatch_palette_item(
        &mut self,
        item: &PaletteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Record an attempted choice before dispatch, even if that operation
        // later fails. The toggle row is just dismissal and receives no usage bonus.
        let is_toggle = matches!(item, PaletteItem::Action(id, ..) if id.0 == "palette::toggle");
        if !is_toggle {
            self.palette_usage.record(&item.usage_key(), unix_now());
            self.palette_usage_version += 1;
        }
        match item {
            PaletteItem::Action(..) if is_toggle => {}
            PaletteItem::Action(id, ..) => self.dispatch(id, None, window, cx),
            PaletteItem::Theme(name) => {
                // The name is already fully qualified (e.g. "Gruvbox
                // Dark"), which `ThemeService::resolve` matches outright.
                self.services.theme.apply(name, cx);
                self.persist_theme(cx);
            }
            PaletteItem::Scope(name) => {
                // The `scope::<name>` actions' own path, notifying whenever
                // the frame changed (provenance included).
                let name = name.clone();
                let _ = self.load_saved_scope(&name, cx);
            }
        }
    }

    /// Capture the selected item, close and restore focus, then dispatch it.
    /// Keyboard Enter and row clicks share this route so actions opening another
    /// overlay start after the palette is gone. Empty results simply close. Over
    /// a dialog stack, dialog actions may push another entry; other actions run
    /// behind it and restore focus to the top dialog. Tile command-line, find,
    /// stack-list, workspace-switch, and pin actions refuse while a dialog
    /// remains open.
    pub(super) fn commit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
        self.close_palette(window, cx);
        if let Some(item) = selected {
            self.dispatch_palette_item(&item, window, cx);
            // A non-dialog action runs behind the stack and may have moved focus
            // (a tile's own input, the shell root). The top dialog keeps it.
            if self.modal_open() {
                super::dialog::refocus_top(self, window, cx);
            }
        }
    }

    /// Handle bubbled palette keys. Escape closes and Enter commits regardless
    /// of modifiers; shared filtered-list commands move selection and scroll it into
    /// view. Other keys do nothing here and keep propagating for Input text delivery.
    /// The caller's palette branch prevents fallthrough to the shell matcher.
    pub(super) fn handle_palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;

        match ks.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" => self.commit_selected(window, cx),
            // Use shared list navigation for every non-commit key that Input leaves available.
            _ => {
                if let Some(ks) = convert_keystroke(&event.keystroke)
                    && let Some(cmd) = listfilter::nav_command(&ks)
                    && let Some(palette) = self.palette.as_mut()
                {
                    let len = palette.filtered().len();
                    let next = crate::vimnav::apply(palette.selected(), len, cmd);
                    palette.set_selected(next);
                    self.sync_palette_scroll();
                }
            }
        }
    }
}

/// The wall clock as unix seconds, for `palette_usage`'s `now` — read
/// once per palette open and once per palette dispatch, never per frame.
/// A clock before the epoch reads as 0 rather than panicking.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
