//! Keyboard input path: the active key-context stack,
//! keystroke dispatch through the compiled keymap, the palette-toggle
//! keystroke special-case, `handle_key_down`'s per-action routing, and the
//! small `persist_*` helpers a few dispatched actions call to write a
//! runtime change back to the user config layer. Every keystroke passes
//! through this seam before landing in a transient surface or a dispatched
//! action.

use gpui::{App, Context, Focusable as _, KeyDownEvent, Window};
use gpui_component::WindowExt as _;

use crate::actions::ActionId;
use crate::commandline::Prompt;
use crate::keymap::{Binding, KeyContext, MatchResult, UNBOUND_ACTION};
use crate::tiling::{Orientation, apply_workspace_action};
use crate::vimfind;
use crate::{fontsize, theme};
use geode_core::query::AsOf;

use super::keys::convert_keystroke;
#[cfg(feature = "profiling")]
use super::profiling_hook;
use super::{
    ShellView, asof_view, choicedialog, dialog, keybindings_view, objectdialog, picker,
    scope_expr_view, settings_view,
};

/// The notice produced when a stack verb targets a tile outside a stack.
pub(super) const NOT_IN_A_STACK: &str = "not in a stack";

impl ShellView {
    /// Active contexts, outermost first: workspace, then tile and occupant when
    /// an occupant is focused, then palette while open. Used for ordinary matching
    /// and the palette toggle; exclusive input owners bypass ordinary matching.
    pub(super) fn context_stack(&self, cx: &App) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
        if let Some(tile) = self.services.workspaces.active().focused_tile()
            && let Some(o) = self.occupants.get(&tile)
        {
            // The shared tile context binds `/` and `:`. The occupant adds its
            // own context innermost so module-specific bindings can apply.
            stack.push(KeyContext::new("tile"));
            stack.push(o.content.key_context(cx));
        }
        if self.palette.is_some() {
            stack.push(KeyContext::new("palette"));
        }
        stack
    }

    /// Whether the winning single-key binding is `palette::toggle`. Resolve
    /// without feeding the matcher so opening or closing the palette cannot
    /// complete a pending sequence. Later rebindings and explicit unbindings
    /// shadow the builtin toggle just as they do in ordinary matching.
    fn is_palette_toggle(&self, keystroke: &crate::keymap::Keystroke, cx: &App) -> bool {
        let stack = self.context_stack(cx);
        self.single_keystroke_binding(keystroke, &stack)
            .is_some_and(|binding| binding.action.0 == "palette::toggle")
    }

    /// Resolve the last single-keystroke binding whose predicate passes. This
    /// leaves matcher sequences and counts untouched while a text surface owns
    /// the keyboard. An explicit unbind is returned as a winning binding; callers
    /// decide whether to consume it or leave the key to their text input.
    fn single_keystroke_binding(
        &self,
        keystroke: &crate::keymap::Keystroke,
        stack: &[KeyContext],
    ) -> Option<&Binding> {
        self.services.keymap.bindings().iter().rfind(|binding| {
            binding.keystrokes.len() == 1
                && binding.keystrokes[0] == *keystroke
                && binding.predicate.as_ref().is_none_or(|p| p.eval(stack))
        })
    }

    /// Dispatch a resolved action from either the keymap or palette. Workspace
    /// actions go through `apply_workspace_action`; shell actions are handled
    /// here, and remaining ids reach the focused occupant with their count.
    ///
    /// Recognized workspace actions, including no-ops, mark the session dirty
    /// and reconcile focus. Persistence runs later on the background save path
    /// so repeated keyboard commands never wait for filesystem writes.
    pub(super) fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Record every dispatch before routing so the crash report includes actions
        // that no handler recognizes. The tail stores hashes without allocating per key.
        self.services
            .action_tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&action.0);
        // The one line every dispatched action leaves in the daily log
        // (`[log] shell = "debug"`): which action, with what count. The
        // branch that resolved it says so on its own line just before.
        tracing::debug!(target: "geode::shell", action = %action.0, count = ?count, "dispatch");

        // Notices and the stack list expire on the next dispatch. The list handles
        // its own bare keys before dispatch, so only external actions close it here.
        self.notice = None;
        self.stack_list = None;
        self.add_filter_menu = None;

        if action.0 == "stack::next" || action.0 == "stack::prev" {
            // Stack cycling consumes the count before the count-free workspace router.
            let n = i64::from(count.unwrap_or(1).max(1));
            let delta = if action.0 == "stack::next" { n } else { -n };
            if self.services.workspaces.active_mut().stack_step(delta) {
                self.session_dirty = true;
                self.note_keyboard_focus_move(window, cx);
            } else {
                self.notice = Some(NOT_IN_A_STACK);
            }
            return;
        }
        if action.0 == "stack::unstack" {
            let rect = self
                .services
                .workspaces
                .active()
                .focused_tile_rect(super::render::content_area(window));
            let orientation = self.add_direction.resolve(None, rect);
            if self
                .services
                .workspaces
                .active_mut()
                .unstack_focused(orientation)
            {
                self.session_dirty = true;
                self.note_keyboard_focus_move(window, cx);
            } else {
                self.notice = Some(NOT_IN_A_STACK);
            }
            return;
        }
        if action.0 == "stack::pick" {
            match self.services.workspaces.active().focused_tile() {
                Some(tile) => self.open_stack_list(tile, window, cx),
                None => self.notice = Some(NOT_IN_A_STACK),
            }
            return;
        }

        // The workspace router ignores counts; the module fallback receives them.
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if handled {
            self.session_dirty = true;
            // Reconcile focus after every recognized workspace action, including no-ops
            // and geometry-only changes. A separate list of focus-moving action ids
            // would have to track the workspace router exactly.
            self.note_keyboard_focus_move(window, cx);
        } else if action.0 == "palette::toggle" {
            self.toggle_palette(window, cx);
        } else if action.0 == "settings::open" {
            // Settings opens through the shared modal owner with its own key handler.
            settings_view::open(self, window, cx);
        } else if action.0 == "keybindings::open" {
            // The keybinding editor is palette-reachable and has no builtin shortcut.
            keybindings_view::open(self, window, cx);
        } else if action.0 == "config::views" {
            // Open the Views browse list. Configuration editors are palette-reachable
            // without builtin shortcuts.
            objectdialog::render::open(self, objectdialog::Domain::Views, window, cx);
        } else if action.0 == "config::groupings" {
            objectdialog::render::open(self, objectdialog::Domain::Groupings, window, cx);
        } else if action.0 == "config::scopes" {
            objectdialog::render::open(self, objectdialog::Domain::Scopes, window, cx);
        } else if action.0 == "config::schema" {
            objectdialog::render::open(self, objectdialog::Domain::Schema, window, cx);
        } else if action.0 == "config::sources" {
            objectdialog::render::open(self, objectdialog::Domain::Sources, window, cx);
        } else if action.0 == "config::colors" {
            objectdialog::render::open(self, objectdialog::Domain::Colors, window, cx);
        } else if action.0 == "fontsize::increase" {
            // Clamped steps (ctrl+= / ctrl+-); render applies the rem size
            // on the notify, persistence mirrors the settings control's
            // set_font_size path.
            self.font_size = self.font_size.larger();
            self.persist_font_size(cx);
        } else if action.0 == "fontsize::decrease" {
            self.font_size = self.font_size.smaller();
            self.persist_font_size(cx);
        } else if action.0 == "config::open_directory" {
            self.open_config_directory(cx);
        } else if action.0 == "ui::line_numbers_cycle" {
            // Cycle off → on → relative → off through the same setter as Settings.
            self.set_line_numbers(self.line_numbers.next(), cx);
        } else if action.0 == "perf::toggle_overlay" {
            // Toggle the readout only; frame-time recording continues while hidden.
            self.perf_overlay = !self.perf_overlay;
            cx.notify();
        } else if action.0 == "tile::command_line" {
            self.open_command_line(Prompt::Command, window, cx);
        } else if action.0 == "tile::find" {
            self.open_command_line(Prompt::Find, window, cx);
        } else if action.0 == "perf::reset" {
            // Zero the frame-time counters so a measurement can start
            // from a known point (e.g. right before an interaction worth
            // profiling). Notify so a visible overlay repaints its
            // zeroed numbers immediately.
            self.perf.reset();
            self.frame.update(cx, |f, _| f.requery.reset());
            // Also drop the previous render's timestamp: this notify's
            // own render would otherwise measure the interval back to
            // whatever frame was painted before the reset (e.g. the
            // user's reaction time in the palette), landing one stale
            // sample in the freshly zeroed histogram. `None` makes the
            // next render record nothing and become the new baseline
            // instead — the render after that records the first real
            // interval.
            self.last_render_started = None;
            cx.notify();
        } else if let Some(n) = action
            .0
            .strip_prefix("frame::slot_")
            .and_then(|s| s.parse::<u8>().ok())
        {
            // Activate a configured grouping slot. Empty slots leave the frame and
            // following tiles unchanged and do not notify.
            self.frame.update(cx, |f, cx| {
                if f.set_active_slot(Some(n)) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::slot_clear" {
            // ctrl+0: return every following tile to its view's own
            // grouping.
            self.frame.update(cx, |f, cx| {
                if f.set_active_slot(None) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::scope_undo" {
            // Walk the bounded scope undo stack.
            self.frame.update(cx, |f, cx| {
                if f.undo_scope() {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::scope_redo" {
            // mod+shift+z: walk the redo stack; cleared by the next
            // `set_scope`/`set_scope_in_session`.
            self.frame.update(cx, |f, cx| {
                if f.redo_scope() {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::scope_clear" {
            // Palette-only (no chord — occasional deliberate act, not
            // muscle memory): clear the whole scope, itself undoable.
            self.frame.update(cx, |f, cx| {
                if f.clear_scope() {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::focus_text" {
            // Focus the scope bar text field.
            self.focus_text_field(window, cx);
        } else if action.0 == "frame::pick" {
            // Open the dimension picker on its column stage.
            picker::open(self, None, window, cx);
        } else if let Some(column) = action.0.strip_prefix("frame::pick_") {
            // A per-column `frame::pick_<column>` action
            // (`defaults::register_pick_actions`) — unbound by default,
            // palette-reachable as "Pick: <column>", or bindable by a
            // user keymap. Opens the picker straight onto that column's
            // values stage.
            picker::open(self, Some(column.to_string()), window, cx);
        } else if action.0 == "scope::save_current" {
            // `scope::save_current` must precede the generic `scope::` prefix match,
            // otherwise it would load a scope named `save_current`. That name is reserved
            // by the Scopes domain. Seed the naming prompt from the current frame scope.
            objectdialog::render::open_save_scope(self, window, cx);
        } else if let Some(name) = action.0.strip_prefix("scope::") {
            // Load a saved scope through `Frame::set_scope`, making the change undoable.
            // These per-scope actions are palette-reachable and bindable by user keymaps.
            self.frame.update(cx, |f, cx| {
                if let Ok(true) = f.load_scope(name) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::as_of" {
            // Open the as-of selector.
            asof_view::open(self, window, cx);
        } else if action.0 == "frame::scope_expression" {
            // Open the frame-expression editor on the whole expression.
            scope_expr_view::open(self, scope_expr_view::Mode::Whole, window, cx);
        } else if action.0 == "frame::add_expression" {
            // Open the expression editor in add mode: the typed expression is
            // joined to the current one with `and` (the `+` menu's Expression row).
            scope_expr_view::open(self, scope_expr_view::Mode::Add, window, cx);
        } else if action.0 == "frame::clear_expression" {
            // Drop the whole expression layer through the undoable set_scope path.
            self.frame.update(cx, |f, cx| {
                if f.clear_expression() {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::grouping" {
            // Open the same grouping picker as the toolbar readout.
            choicedialog::open_grouping(self, window, cx);
        } else if action.0 == "tile::add" {
            // Open the same tile-kind picker as a placeholder double-click.
            choicedialog::open_tile_kinds(self, window, cx);
        } else if action.0 == "log::level" {
            // Open the target-then-level picker for application logging.
            choicedialog::open_log_level(self, window, cx);
        } else if action.0 == "frame::live" {
            // Return to live and retain the previous as-of for `frame::as_of_undo`.
            self.frame.update(cx, |f, cx| {
                if f.set_as_of(AsOf::Live) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::as_of_undo" {
            // Palette-only: swap back to the previous as-of — a toggle,
            // not a stack (see `Frame::undo_as_of`).
            self.frame.update(cx, |f, cx| {
                if f.undo_as_of() {
                    cx.notify();
                }
            });
        } else if let Some((kind, placement)) = crate::defaults::parse_add_action(&action.0) {
            // Add or fill a tile using the action's placement. Split follows the setting;
            // explicit placements and stacking override it. This never focuses an
            // existing tile of that kind; `open_module` owns that behavior.
            let kind = kind.to_string();
            self.add_tile(&kind, placement, None, window, cx);
        } else if action.0 == "workspace::duplicate_horizontal" {
            self.duplicate_tile(Orientation::Horizontal, window, cx);
        } else if action.0 == "workspace::duplicate_vertical" {
            self.duplicate_tile(Orientation::Vertical, window, cx);
        } else {
            // Profiler-feature actions (`perf::dump`, `perf::gpui_overlay`)
            // — compiled (and registered) only with the `profiling`
            // feature; see `shell::profiling_hook`. Returns whether it
            // recognised the id, so the module fall-through below still
            // runs for anything it didn't claim.
            #[cfg(feature = "profiling")]
            if profiling_hook::dispatch(self, action, window, cx) {
                return;
            }
            // Offer remaining ids to the focused occupant. Unhandled ids have no effect.
            if let Some(tile) = self.services.workspaces.active().focused_tile()
                && let Some(o) = self.occupants.get(&tile)
            {
                o.content.dispatch(action, count, window, cx);
            }
        }
    }

    /// Persist the live theme name to the user `app.toml` through the directory
    /// FIFO. Submission happens before background execution so rapid choices
    /// persist in UI order; the read-modify-write preserves unrelated keys.
    ///
    /// Missing user configuration directories skip persistence. Write failures
    /// are logged as `geode::theme` warnings and do not roll back the live theme.
    /// No filesystem work runs on the UI thread.
    pub(super) fn persist_theme(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let name = self.services.theme.active_name().to_string();
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = theme::persist_to_user_config(&dir, &name) {
                tracing::warn!(target: "geode::theme", "{e}");
            }
        })
        .detach();
    }

    /// Persist the current font size to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_theme`] just above (missing `user_dir` = silently skipped;
    /// failures are a `geode::config` warning; writes run in submission order).
    pub(super) fn persist_font_size(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let size = self.font_size;
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = fontsize::persist_to_user_config(&dir, size) {
                // Include the setting in the warning: font size and find style share
                // `app.toml` and can return identical parse errors on the same log target.
                tracing::warn!(target: "geode::config", "font size not saved: {e}");
            }
        })
        .detach();
    }

    /// `config::open_directory`: open the user config directory in the
    /// OS file manager. `open_with_system` (the platform's `open` /
    /// `ShellExecute`) opens a directory as a window; `reveal_path` would
    /// only select it inside its parent. The directory is created first
    /// so a fresh install with no `~/.config/geode` yet gets a window
    /// rather than a silent failure — the same `create_dir_all` every
    /// `config_write` does — on the background executor, since nothing
    /// blocks the render thread; the open itself needs `App` and runs
    /// back on the foreground once the directory exists.
    fn open_config_directory(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            tracing::warn!(
                target: "geode::config",
                "config directory not opened: no user config directory (HOME/APPDATA unset)"
            );
            return;
        };
        cx.spawn(async move |_this, cx| {
            let created = cx
                .background_executor()
                .spawn({
                    let dir = dir.clone();
                    async move { std::fs::create_dir_all(&dir) }
                })
                .await;
            if let Err(e) = created {
                tracing::warn!(
                    target: "geode::config",
                    "config directory not opened: failed to create {}: {e}",
                    dir.display()
                );
                return;
            }
            cx.update(|cx| cx.open_with_system(&dir));
        })
        .detach();
    }

    /// Persist the current find style to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_font_size`] just above (missing `user_dir` = silently
    /// skipped; failures are a `geode::config` warning; writes run in submission order).
    pub(super) fn persist_find_style(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let style = self.find_style;
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = vimfind::persist_to_user_config(&dir, style) {
                // Identify the setting so a shared `app.toml` parse error is actionable.
                tracing::warn!(target: "geode::config", "find style not saved: {e}");
            }
        })
        .detach();
    }

    /// Set `[ui] line_numbers`, publish it to every module through the
    /// `linenumbers::UiSettings` global (which fires their
    /// `observe_global` subscriptions), persist it and repaint. The one
    /// setter both the settings row and `ui::line_numbers_cycle` go
    /// through; a hot reload writes the field and the global itself,
    /// since it must not persist what it just read.
    pub(crate) fn set_line_numbers(
        &mut self,
        mode: crate::linenumbers::LineNumbers,
        cx: &mut Context<Self>,
    ) {
        self.line_numbers = mode;
        cx.set_global(crate::linenumbers::UiSettings { line_numbers: mode });
        self.persist_line_numbers(cx);
        cx.notify();
    }

    /// Persist `[ui] line_numbers`, off the UI thread — the exact
    /// contract of [`Self::persist_find_style`] above.
    pub(super) fn persist_line_numbers(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let mode = self.line_numbers;
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = crate::linenumbers::persist_to_user_config(&dir, mode) {
                tracing::warn!(target: "geode::config", "line numbers not saved: {e}");
            }
        })
        .detach();
    }

    /// Set `[timeseries] default_source`, publish it through the
    /// `series::SeriesSettings` global (the fetch-source list is
    /// re-derived with it — sources are restart-gated, so it is
    /// unchanged in practice), persist and repaint. The settings row's
    /// one setter; a hot reload writes the field and the global itself,
    /// since it must not persist what it just read.
    pub(crate) fn set_default_source(&mut self, source: Option<String>, cx: &mut Context<Self>) {
        self.default_source = source.clone();
        let mut series = crate::series::SeriesSettings::from_config(&self.services.config);
        series.default_source = source;
        cx.set_global(series);
        self.persist_default_source(cx);
        cx.notify();
    }

    /// Persist `[timeseries] default_source`, off the UI thread — the
    /// exact contract of [`Self::persist_find_style`] above; `(none)` is
    /// `None`, which REMOVES the key.
    pub(super) fn persist_default_source(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let source = self.default_source.clone();
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = crate::series::persist_to_user_config(&dir, source.as_deref()) {
                tracing::warn!(target: "geode::config", "default source not saved: {e}");
            }
        })
        .detach();
    }

    /// Persist `[tiles] add`, off the UI thread — the exact contract of
    /// [`Self::persist_find_style`] just above.
    pub(super) fn persist_add_direction(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let direction = self.add_direction;
        crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            if let Err(e) = crate::tileadd::persist_to_user_config(&dir, direction) {
                tracing::warn!(target: "geode::config", "add direction not saved: {e}");
            }
        })
        .detach();
    }

    /// Focus the scope bar's live text field. Its Focus subscription opens the
    /// scope-editing session and records the entry text.
    pub(super) fn focus_text_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.filter_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        cx.notify();
    }

    /// After a chord dispatched from inside the focused text field: if the
    /// field still has focus and the frame's text no longer matches what
    /// it shows, show the frame's. See the chord branch of
    /// `handle_key_down` for why this is the one focused-field write.
    fn reflect_frame_text_into_focused_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            return;
        }
        let frame_text = self.frame.read(cx).scope().text.clone().unwrap_or_default();
        if self.filter_input.read(cx).value().as_ref() != frame_text.as_str() {
            self.filter_input.update(cx, |i, cx| {
                i.set_value(frame_text, window, cx);
            });
        }
    }

    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Both shell modals and component dialogs exclude the ordinary matcher,
        // preventing actions from reaching tiles behind the overlay. A shell modal
        // gets first refusal through its key handler; an unclaimed Escape closes it.
        // Component dialogs retain their own handling.
        if self.modal_open() || window.has_active_dialog(cx) {
            if self.modal_open() {
                // Clone the handler before calling it so the modal borrow ends before
                // the closure receives mutable access to the shell.
                let handler = self.modals.last().and_then(|m| m.on_key.clone());
                let handled = handler.is_some_and(|handler| {
                    convert_keystroke(&event.keystroke)
                        .is_some_and(|ks| handler(self, &ks, window, cx))
                });
                // Reconcile the shared input and focus after every modal handler call,
                // even for unclaimed keys. A handler may change state without consuming
                // a key. Once closed, the modal's closer owns focus instead.
                if self.modal_open() {
                    dialog::sync_dialog_text(self, window, cx);
                }
                if handled {
                    // Claimed keys must not also reach text insertion. Unclaimed printable
                    // keys keep propagating so the focused filter can receive them.
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "escape" {
                    self.close_modal(window, cx);
                }
            }
            return;
        }

        // A focused command line handles its own keys before other text owners.
        // Unclaimed keys propagate to its Input without reaching the matcher.
        if self.command_line.is_some()
            && self
                .command_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            // The effective palette toggle remains available from the command line.
            // Opening the palette cancels the line through the same transition used
            // by other palette entry points, respecting user rebindings.
            if let Some(ks) = convert_keystroke(&event.keystroke)
                && self.is_palette_toggle(&ks, cx)
            {
                self.toggle_palette(window, cx);
                cx.notify();
                return;
            }
            if self.handle_command_line_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }

        // While the scope text field is focused, ordinary typing bypasses the
        // matcher. Only chords (Control, Alt, or Command; Shift alone is typing)
        // resolve single-key bindings, against the workspace context alone. This
        // keeps tile motions from running behind the field and prevents typed
        // numbers from becoming matcher counts. Input-owned editing shortcuts
        // are consumed by the component before this listener.
        //
        // A dispatched chord is consumed so it cannot also insert a character. If
        // the action changes the frame text while focus stays here, reflect that
        // result into the field. `set_value` emits no Change event, avoiding a
        // feedback loop. Escape restores the entry text and returns shell focus.
        if self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            if let Some(ks) = convert_keystroke(&event.keystroke)
                && ks.mods.is_chord()
            {
                let stack = [KeyContext::new("workspace")];
                let action = self
                    .single_keystroke_binding(&ks, &stack)
                    .map(|binding| binding.action.clone());
                tracing::debug!(
                    target: "geode::shell",
                    key = ?ks,
                    resolved = ?action.as_ref().map(|a| a.0.as_str()),
                    "key: filter-field chord"
                );
                if let Some(action) = action {
                    if action.0 != UNBOUND_ACTION {
                        self.dispatch(&action, None, window, cx);
                        self.reflect_frame_text_into_focused_field(window, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
            }
            if event.keystroke.key == "escape" {
                // Restore text before ending the scope session. Reverting inside the
                // session avoids pushing the abandoned typed scope onto undo history.
                // `set_value` emits no Change event, so the model is updated explicitly.
                if let Some(base) = self.filter_session_base.take() {
                    self.filter_input.update(cx, |i, cx| {
                        i.set_value(base.clone(), window, cx);
                    });
                    self.frame.update(cx, |f, cx| {
                        let mut reverted = f.scope().clone();
                        reverted.text = (!base.trim().is_empty()).then_some(base);
                        let changed = f.set_scope_in_session(reverted);
                        f.end_scope_session();
                        if changed {
                            cx.notify();
                        }
                    });
                }
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
            return;
        }

        // An occupant holding keyboard focus in insert mode bypasses matcher
        // sequences and counts. Chords resolve against the full context stack;
        // bare keys resolve only against contexts carrying `mode == insert`.
        // Thus module Enter/Escape bindings can commit or cancel, while `/`, `:`,
        // letters, and digits remain text rather than background shell commands.
        //
        // `occupant_insert_stack` checks both structural and keyboard ownership.
        // The render focus-restoration path uses the same predicate, preventing
        // keys in one tile's abandoned field from routing to another tile. When
        // an editor closes, it must blur before dropping its InputState: Root
        // retains the focused input, so dropping the module's handle is insufficient.
        if let Some(stack) = self.occupant_insert_stack(window, cx) {
            let ks = convert_keystroke(&event.keystroke);
            let resolved = ks.as_ref().and_then(|ks| {
                self.single_keystroke_binding(ks, &insert_contexts(&stack, ks))
                    .map(|binding| binding.action.clone())
            });
            // Debug-level, never formatted unless `geode::shell` is at
            // `debug`: which key reached the insert branch, whether it
            // came in as a chord (resolved against the whole stack) or
            // bare (insert contexts only), and what it resolved to — the
            // question "why did this key type / not type" is answered
            // from the daily log rather than by guessing.
            tracing::debug!(
                target: "geode::shell",
                key = ?ks,
                chord = ks.as_ref().is_some_and(|k| k.mods.is_chord()),
                resolved = ?resolved.as_ref().map(|a| a.0.as_str()),
                "key: insert branch"
            );
            if let Some(action) = resolved
                && action.0 != UNBOUND_ACTION
            {
                self.dispatch(&action, None, window, cx);
                // Stopped for the same reason the filter field stops a
                // dispatched chord: a claimed keystroke must not ALSO
                // reach the window's text-input phase and type itself
                // into the input behind the action (gpui runs that
                // phase only while the event still propagates).
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        // The palette handles its own navigation and text below. Convert once for
        // both the direct toggle check and ordinary matching.
        let keystroke = convert_keystroke(&event.keystroke);

        if let Some(ks) = &keystroke
            && self.is_palette_toggle(ks, cx)
        {
            self.toggle_palette(window, cx);
            cx.notify();
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(event, window, cx);
            cx.notify();
            return;
        }

        if self.add_filter_menu.is_some() {
            // The add-a-filter menu consumes every bare key; a chord passes to the
            // matcher below, and its dispatch closes the menu.
            let is_chord = convert_keystroke(&event.keystroke).is_some_and(|ks| ks.mods.is_chord());
            if self.handle_add_filter_key(event.keystroke.key.as_str(), is_chord, window, cx) {
                return;
            }
        }

        if let Some(list) = self.stack_list.clone() {
            // Chords pass through to the matcher; they are never member-list digits
            // or motions. Dispatch closes the list. The palette toggle above can
            // open the palette directly and close the list through that transition.
            let is_chord = convert_keystroke(&event.keystroke).is_some_and(|ks| ks.mods.is_chord());
            if !is_chord {
                // The member list consumes every non-chord key. Motions wrap, digits
                // activate immediately, Enter activates the highlight, and Escape closes
                // without changing the active member.
                let key = event.keystroke.key.as_str();
                match key {
                    "escape" => self.close_stack_list(cx),
                    "j" | "down" => {
                        let mut l = list;
                        super::stacklist::step(&mut l, 1);
                        self.stack_list = Some(l);
                    }
                    "k" | "up" => {
                        let mut l = list;
                        super::stacklist::step(&mut l, -1);
                        self.stack_list = Some(l);
                    }
                    "enter" => {
                        if let Some(id) = list.members.get(list.highlighted).copied() {
                            self.activate_stack_member(id, window, cx);
                        }
                    }
                    d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
                        if let Some(id) =
                            super::stacklist::jump(&list, u32::from(d.as_bytes()[0] - b'0'))
                        {
                            self.activate_stack_member(id, window, cx);
                        }
                    }
                    _ => {}
                }
                cx.notify();
                return;
            }
        }

        // Escape ends either drag before ordinary matching. Tile drag cancels
        // without applying a move; divider drag retains its live resizes and marks
        // the session dirty. Armed drags are covered too. Earlier modal and palette
        // routes retain priority for their own Escape handling.
        if event.keystroke.key == "escape"
            && (self.tile_drag.is_some() || self.divider_drag.is_some())
        {
            self.cancel_tile_drag();
            self.cancel_divider_drag();
            cx.notify();
            return;
        }

        let Some(keystroke) = keystroke else {
            return;
        };
        let stack = self.context_stack(cx);
        let result = self
            .matcher
            .press(&self.services.keymap, keystroke.clone(), &stack);
        // The matcher's answer beside the stack it was asked against —
        // the context names and their `mode` pairs are what decide a
        // binding, so a surprising resolution reads straight off this line.
        tracing::debug!(
            target: "geode::shell",
            key = ?keystroke,
            result = ?result,
            stack = ?stack,
            "key: matcher"
        );
        match result {
            MatchResult::Matched { action, count } => {
                self.dispatch(&action, count, window, cx);
                cx.notify();
            }
            MatchResult::Pending | MatchResult::NoMatch => {
                // Repaint pending-sequence hints even when no action was dispatched.
                cx.notify();
            }
        }
    }
}

/// Insert-mode contexts: chords use the whole stack; bare keys use only
/// contexts carrying `mode == insert`. This keeps shell text-like bindings
/// from firing while allowing the occupant's explicit editing controls.
fn insert_contexts<'a>(
    stack: &'a [KeyContext],
    keystroke: &crate::keymap::Keystroke,
) -> std::borrow::Cow<'a, [KeyContext]> {
    if keystroke.mods.is_chord() {
        std::borrow::Cow::Borrowed(stack)
    } else {
        std::borrow::Cow::Owned(
            stack
                .iter()
                .filter(|c| c.get("mode") == Some("insert"))
                .cloned()
                .collect(),
        )
    }
}
