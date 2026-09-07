//! Keyboard input path (spec section 3, 4): the active key-context stack,
//! keystroke dispatch through the compiled keymap, the palette-toggle
//! keystroke special-case, `handle_key_down`'s per-action routing, and the
//! small `persist_*` helpers a few dispatched actions call to write a
//! runtime change back to the user config layer. Split out of
//! `shell/mod.rs` (Phase 3c Task 0) as the seam every keystroke passes
//! through before landing in `palette_ctl`, `commandline_ctl`, `drag`, or
//! a dispatched action.

use gpui::{App, Context, Focusable as _, KeyDownEvent, Window};
use gpui_component::WindowExt as _;

use crate::actions::ActionId;
use crate::commandline::Prompt;
use crate::keymap::{KeyContext, MatchResult};
use crate::tiling::apply_workspace_action;
use crate::vimfind;
use crate::{fontsize, theme};
use geode_core::query::AsOf;

use super::keys::convert_keystroke;
use super::{ShellView, asof_view, keybindings_view, picker, settings_view};

impl ShellView {
    /// The active context stack for key resolution, outermost first:
    /// `workspace` is always active; `palette` layers on top while open.
    /// Currently only consulted by [`is_palette_toggle`](Self::is_palette_toggle)
    /// (to gate that binding's own `context`, if a user keymap ever adds
    /// one) — `handle_key_down` never reaches `self.matcher.press` while
    /// `self.palette` is `Some`, since palette-open key handling is
    /// exclusive (see that method's doc comment).
    pub(super) fn context_stack(&self, cx: &App) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
        if let Some(tile) = self.services.workspaces.active().focused_tile()
            && let Some(o) = self.occupants.get(&tile)
        {
            // `tile` is the shell's own frame for "some occupant has
            // focus" (Task 5 binds `/` and `:` on it); the occupant's own
            // context sits above it, innermost.
            stack.push(KeyContext::new("tile"));
            stack.push(o.content.key_context(cx));
        }
        if self.palette.is_some() {
            stack.push(KeyContext::new("palette"));
        }
        stack
    }

    /// True if `keystroke` exactly matches a single-key binding for
    /// `palette::toggle`. Checked directly against the keymap rather than
    /// through `self.matcher`, so the toggle key can open *and* close the
    /// palette without ever touching (or being confused by) the matcher's
    /// own pending-sequence state, which palette-open key handling
    /// bypasses entirely.
    ///
    /// The keymap's layering contract is last-exact-match-wins (mirrors
    /// `Matcher::press`, spec §3.4): among every single-keystroke binding
    /// for this exact key whose predicate passes the current context
    /// stack, the *last* one in `Keymap::bindings()`'s layer-then-
    /// declaration order is the one that actually governs the key — a
    /// user/desk layer rebinding or unbinding (`"ctrl+k" = "none"`) it must
    /// shadow the builtin `palette::toggle` binding here exactly as it
    /// would through the matcher. So this resolves that same winning
    /// binding and only treats the keystroke as the palette toggle when
    /// its action is `palette::toggle`.
    fn is_palette_toggle(&self, keystroke: &crate::keymap::Keystroke, cx: &App) -> bool {
        let stack = self.context_stack(cx);
        let winner = self.services.keymap.bindings().iter().rfind(|binding| {
            binding.keystrokes.len() == 1
                && binding.keystrokes[0] == *keystroke
                && binding.predicate.as_ref().is_none_or(|p| p.eval(&stack))
        });
        winner.is_some_and(|binding| binding.action.0 == "palette::toggle")
    }

    /// Apply one resolved action id through the shell's one dispatch chain
    /// (spec: "one keymap, ours" — no parallel action-dispatch system).
    /// Workspace verbs go through `apply_workspace_action`; the shell's own
    /// non-workspace actions (`palette::toggle`, `theme::toggle_mode`) are
    /// handled here when that leaves them unhandled. Shared by the normal
    /// keymap-matcher path and the palette's Enter-to-dispatch path, so
    /// both take exactly the same action to the same place.
    ///
    /// Every successful workspace-mutating dispatch marks the session dirty
    /// (Task 3, plan constraint: "session saves happen inside the dispatch
    /// path after workspace-mutating actions, post-action, not per frame"
    /// — the constraint's own documented alternative: "a save-on-mutation-
    /// with-boolean-dirty-flag flushed by the watcher's 500ms tick").
    /// `apply_workspace_action` returning `true` means the action was
    /// recognized as a workspace verb (see its own doc comment: this
    /// includes no-op edge cases like focusing past the last tile) —
    /// marking dirty on every one of those, not just the ones that actually
    /// changed geometry, keeps this a single cheap flag-set instead of a
    /// second "did anything really change" comparison. The actual write
    /// happens later, off the UI thread, on the background watcher's
    /// ~500ms tick (see `new`'s loop and `take_dirty_session_write`) — Task
    /// 3 fix round 1: writing synchronously here, once per dispatch, could
    /// stall the render thread under OS key-repeat on a slow filesystem.
    pub(super) fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every workspace verb below ignores the count; only the module
        // fall-through at the end (Phase 3 §3.3) is count-aware today.
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if handled {
            self.session_dirty = true;
        } else if action.0 == "palette::toggle" {
            self.toggle_palette(window, cx);
        } else if action.0 == "theme::toggle_mode" {
            self.services.theme.toggle_mode(cx);
            self.persist_theme(cx);
        } else if action.0 == "settings::open" {
            // The settings dialog (settings-dialog rewrite: the keybinding
            // dialog's keyboard-driven row-list pattern, home-rolled —
            // `settings_view`'s module doc has the full story; the
            // gpui-component `Settings` composite is gone). Reachable via
            // `ctrl+,`, the palette, and the sidebar profile icon. Goes
            // through `dialog::open_shell_dialog_with_key` via
            // `settings_view::open` itself, so it gets the crate's uniform
            // open-time hygiene plus first refusal on every keystroke.
            settings_view::open(self, window, cx);
        } else if action.0 == "keybindings::open" {
            // Part B: the keybinding dialog itself (vimnav.rs +
            // keymap_edit.rs are its pure cores). Reachable today only via
            // the palette (defaults.rs: no key binding).
            keybindings_view::open(self, window, cx);
        } else if action.0 == "fontsize::increase" {
            // Clamped steps (ctrl+= / ctrl+-); render applies the rem size
            // on the notify, persistence mirrors the settings control's
            // set_font_size path.
            self.font_size = self.font_size.larger();
            self.persist_font_size(cx);
        } else if action.0 == "fontsize::decrease" {
            self.font_size = self.font_size.smaller();
            self.persist_font_size(cx);
        } else if action.0 == "perf::toggle_overlay" {
            // Spec §7.4's debug readout toggle. Display-only — the
            // histogram records regardless (see `render`'s top) — so the
            // toggle is just a bool flip plus a repaint.
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
            // ctrl+1..9 (§4.2): activate a configured slot. An empty slot
            // is ignored — `set_active_slot` returns `false` and nothing
            // notifies, so the readout and every following tile stay put.
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
            // mod+z (spec §3.6): walk the bounded undo stack.
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
            // mod+/ (spec §3.11): focus the scope bar's live text field
            // from anywhere in the shell.
            self.focus_text_field(window, cx);
        } else if action.0 == "frame::pick" {
            // mod+p (spec §3.3): the two-stage dimension picker, opened on
            // the column-choice stage.
            picker::open(self, None, window, cx);
        } else if let Some(column) = action.0.strip_prefix("frame::pick_") {
            // A per-column `frame::pick_<column>` action
            // (`defaults::register_pick_actions`) — unbound by default,
            // palette-reachable as "Pick: <column>", or bindable by a
            // user keymap. Opens the picker straight onto that column's
            // values stage.
            picker::open(self, Some(column.to_string()), window, cx);
        } else if let Some(name) = action.0.strip_prefix("scope::") {
            // A per-scope `scope::<name>` action (`defaults::
            // register_scope_actions`, spec §3.11) — unbound by default,
            // palette-reachable as "Scope: <name>", or bindable by a user
            // keymap. Loads the named saved scope, undoable like any
            // other scope change (`Frame::load_scope` goes through
            // `set_scope`) — same pattern as the palette's own
            // `PaletteItem::Scope` handler (`palette_ctl.rs`).
            self.frame.update(cx, |f, cx| {
                if let Ok(true) = f.load_scope(name) {
                    cx.notify();
                }
            });
        } else if action.0 == "frame::as_of" {
            // mod+t (spec §3.6): the as-of selector modal.
            asof_view::open(self, window, cx);
        } else if action.0 == "frame::live" {
            // Palette-only (spec §3.6, same reasoning as `frame::
            // scope_clear`): return to live, remembering the previous
            // as-of for `frame::as_of_undo` to swap back to.
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
            // A module's own action (§3.3): hand it to the focused
            // occupant. Unhandled ids fall off the end silently, as they
            // always did.
            if let Some(tile) = self.services.workspaces.active().focused_tile()
                && let Some(o) = self.occupants.get(&tile)
            {
                o.content.dispatch(action, count, window, cx);
            }
        }
    }

    /// The apply-then-persist seam every UI theme-change path calls right
    /// after applying a change live through `ThemeService` (`theme::
    /// toggle_mode` above, `dispatch_palette_item`'s `Theme` branch below,
    /// and `settings_view::set_theme`/`set_dark_mode`) — one place that
    /// knows how to turn "the active theme just changed" into a write of
    /// `<user_dir>/app.toml`'s `[theme]` table (`theme::
    /// persist_to_user_config`), so a theme choice survives a restart via
    /// the ordinary config layer instead of the removed session `theme_mode`
    /// mechanism.
    ///
    /// Review fix round 1, Finding 1: `persist_to_user_config` does real,
    /// potentially-blocking file I/O — a read, a `toml_edit` parse, an
    /// `fsync`, a rename — so it must never run inline on the UI thread
    /// (PHILOSOPHY.md: "nothing may stall the render thread"), the same
    /// reasoning `session::write_atomic` already gets in this file's own
    /// watcher loop (see `new`'s doc comment). Only the cheap part — reading
    /// `user_dir`/`active_name`/`active_mode` off `self` — happens here, on
    /// the UI thread; the actual read-modify-write runs inside a task handed
    /// to `cx.background_executor()`, fire-and-forget (`.detach()`): theme
    /// changes are infrequent (a user action, not a hot path like key-repeat),
    /// so there's no need for the session-save path's coalescing
    /// dirty-flag/watcher-tick machinery here — a plain spawn per change is
    /// simple and cheap enough. A write failure surfaces as a `[theme]
    /// warning:` stderr line from inside the task, never a crash — same
    /// convention as every other config-write failure in this codebase.
    ///
    /// A missing `user_dir` (no writable user config dir — some test setups,
    /// or a platform with neither `$HOME` nor `%APPDATA%`) is a silent
    /// no-op, checked before ever spawning: theme persistence, like session
    /// persistence, is best-effort, never load-bearing.
    ///
    /// Raciness (Finding 1, acknowledged rather than engineered away): two
    /// theme changes in quick succession spawn two independent
    /// read-modify-write tasks against the same `app.toml`, with no
    /// ordering guarantee between them beyond however the background
    /// executor happens to schedule them — whichever task's rename lands
    /// second wins the `[theme]` table. This is last-writer-wins on
    /// `[theme]`, not a torn file (`persist_to_user_config`'s unique
    /// pid+counter temp names already rule that out — each task only ever
    /// touches its own temp file until its own rename), and in the
    /// vanishingly rare case it's even reachable — two theme changes inside
    /// one background-executor scheduling window — the on-disk value still
    /// converges to *some* real, valid theme choice, never a corrupt one.
    /// Accepted as-is rather than serialized through a dirty-flag/watcher-
    /// tick queue: theme changes are rare UI actions, not the key-repeat-
    /// speed churn the session path was built to survive.
    pub(super) fn persist_theme(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let name = self.services.theme.active_name().to_string();
        let mode = self.services.theme.active_mode();
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = theme::persist_to_user_config(&dir, &name, mode) {
                    eprintln!("[theme] warning: {e}");
                }
            })
            .detach();
    }

    /// Persist the current font size to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_theme`] just above (missing `user_dir` = silently skipped;
    /// failures are a stderr warning; last-write-wins races accepted for
    /// the same rare-UI-action reasons).
    pub(super) fn persist_font_size(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let size = self.font_size;
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = fontsize::persist_to_user_config(&dir, size) {
                    eprintln!("[fontsize] warning: {e}");
                }
            })
            .detach();
    }

    /// Persist the current find style to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_font_size`] just above (missing `user_dir` = silently
    /// skipped; failures are a stderr warning; last-write-wins races
    /// accepted for the same rare-UI-action reasons).
    pub(super) fn persist_find_style(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let style = self.find_style;
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = vimfind::persist_to_user_config(&dir, style) {
                    eprintln!("[findstyle] warning: {e}");
                }
            })
            .detach();
    }

    /// `mod+/` (spec §3.11): move focus into the scope bar's live text
    /// field from anywhere in the shell. The field's own `InputEvent::
    /// Focus` subscription (`ShellView::new`) is what actually opens the
    /// scope-editing session — this just moves the focus handle.
    pub(super) fn focus_text_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.filter_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Task 9 instant-modal redesign (see `dialog`'s module doc): while
        // Geode's own modal (`self.modal`) OR a gpui-component `Dialog`
        // layer is open, the shell's own keymap `Matcher` must not see a
        // single keystroke — otherwise e.g. `ctrl+v` typed inside the
        // settings dialog would *also* dispatch `workspace::split_right`
        // behind it (the modal paints above the tile surface, but this
        // on_key_down listener sits on the ShellView root and still
        // receives every raw KeyDownEvent that bubbles up the dispatch
        // tree, modal-focused or not — same "delivered regardless"
        // behavior the filter-input guard below already relies on).
        // `window.has_active_dialog` is kept alongside
        // `self.modal.is_some()`, not replaced by it: gpui-component's own
        // popovers still open through that crate's dialog-layer machinery,
        // so this guard still accounts for it even though nothing in this
        // crate opens a gpui-component `Dialog` — or, since the settings
        // row-list rewrite retired the theme dropdown, any of its popover
        // controls — anymore.
        //
        // Escape is the one key this branch still acts on itself — closing
        // our own modal, same as a backdrop click (`dialog::render_modal`).
        // There is no separate "Dialog" action-context to defer to anymore
        // (that was gpui-component's own `Cancel`/`Confirm` action binding,
        // scoped to its dialog's focused root): our modal is plain chrome,
        // not an action-dispatch layer, so this is the only place Escape
        // gets handled for it — after the modal's own `on_key` handler
        // (below) has had first refusal, which is how a dialog swallows an
        // Escape as "cancel" without closing: today that is only the
        // keybinding dialog's rebind capture (`keybindings_view`'s
        // `listening` branch) — the settings dialog claims no keys of its
        // own on Escape, so it always falls through to this branch's
        // close.
        if self.modal.is_some() || window.has_active_dialog(cx) {
            if self.modal.is_some() {
                // Offer the modal's own key handler (if any) first
                // refusal — the keybinding dialog's rebind-capture seam
                // (`dialog::ModalKeyHandler`, see its own doc comment
                // for why this is the shell-native `Keystroke`, not gpui's
                // raw one). `on_key` is cloned out of `self.modal` before
                // being called for the same reentrancy reason `render`
                // clones `title`/`build` out ahead of invoking them (see
                // `ShellModal`'s doc comment): the closure needs `self` back
                // as `&mut ShellView` while `self.modal`'s own borrow must
                // already be released.
                let handler = self.modal.as_ref().and_then(|m| m.on_key.clone());
                let handled = handler.is_some_and(|handler| {
                    convert_keystroke(&event.keystroke)
                        .is_some_and(|ks| handler(self, &ks, window, cx))
                });
                if handled {
                    // A key the modal claimed must not also reach the
                    // window's text-input phase. That phase is what a
                    // focused `Input` actually inserts characters from
                    // (`Window::dispatch_keystroke`: it runs only when the
                    // key event still `propagate`s after every listener),
                    // and the dialogs' shared filter field is focused
                    // whenever a list dialog is open — so without this,
                    // `enter` (`key_char = "\n"`) would land in the filter
                    // right after the dialog acted on it, and the
                    // resulting `InputEvent::Change` would reset the very
                    // state the keystroke just set up. Keys the handler
                    // did NOT claim deliberately keep propagating: that is
                    // how a typed character reaches the filter at all.
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

        // The per-tile command line (§3.4) owns its own key handling
        // while its input has focus — escape/enter/tab/ctrl+n/ctrl+p are
        // claimed by `handle_command_line_key`, everything else falls
        // through to the focused `Input` exactly like the filter field
        // below. Checked ahead of it (never both focused at once, but
        // this is the more specific guard).
        if self.command_line.is_some()
            && self
                .command_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        {
            // The one deliberate exception (fix round 1, finding 1): the
            // palette toggle is a shipped, always-reachable binding — "a
            // binding that has shipped is a promise" — so it must still
            // work from inside the command line, unlike every other
            // keystroke here. Checked first, via the same `is_palette_
            // toggle` the top-level check below uses, so it resolves the
            // identical winning binding (desk/user layers included).
            // `toggle_palette` itself now cancels an open command line
            // unconditionally (its own doc comment), so this one call
            // both opens the palette and cleans up the line.
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

        // The filter field (Task 4) owns its own key handling while it has
        // focus — typing must reach it, not the shell's keymap `Matcher`
        // (brief: "shell chords won't fire — acceptable while typing a
        // filter"). This has to be handled explicitly rather than relying
        // on gpui's dispatch to simply not reach here: gpui-component's
        // `Input` binds most editing keys (typing, backspace, arrows,
        // ctrl+v paste, …) as *actions* scoped to its own key context, but
        // its `Escape` action handler calls `cx.propagate()` whenever there
        // is no popover/inline-completion/IME-marked-text/`clean_on_escape`
        // to consume it (the plain-filter case, always, here) — and any key
        // with *no* action binding at all in that context (e.g. `ctrl+h`,
        // `ctrl+k`, bare typed letters) skips the action system entirely.
        // Both cases still deliver the raw `KeyDownEvent` to every
        // `on_key_down` listener up the dispatch path, this one included
        // (verified against the pinned gpui rev's `Window::
        // finish_dispatch_key_event`/`dispatch_key_down_up_event`), so
        // without this guard e.g. `ctrl+h` typed into the filter would
        // *also* dispatch `workspace::split_down`. Esc is the one key this
        // view still acts on itself: it hands focus back to the shell root
        // so hjkl and friends resume working immediately.
        if self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            if event.keystroke.key == "escape" {
                // Restore the text the field had when it took focus (spec
                // §3.11) — `filter_session_base` is only `Some` while a
                // session is open (set on `InputEvent::Focus`, taken here
                // or on blur/enter). `set_value` emits no `Change` (the
                // checked note in `dialog.rs`), so this cannot re-trigger
                // the per-keystroke subscription.
                //
                // The revert goes through `set_scope_in_session` — still
                // inside the session — and only *then* ends it (fix round
                // 1, Finding 1). The first cut called `end_scope_session`
                // first and reverted through the ordinary `set_text`: that
                // runs outside the session, so it pushed a *second* undo
                // entry — the just-typed, now-abandoned scope — leaving
                // the session's own coalesced entry (the pre-focus scope)
                // buried underneath it. One `undo_scope` then popped the
                // abandoned scope straight back onto the screen — Escape
                // resurrecting exactly the text it had just thrown away.
                // Reverting inside the session instead coalesces the
                // revert into the session's *one* entry (a no-op push,
                // since the session already recorded that same pre-focus
                // scope on the first keystroke), so the abandoned scope
                // is never pushed anywhere and undo cannot reach it.
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

        // Deliberately no analogous "if palette_input is focused, return
        // unless escape" guard here — unlike the filter field above, the
        // palette needs *more* than Escape to reach it while its own input
        // has focus (up/down, ctrl+p/ctrl+n, enter). That routing lives in
        // `handle_palette_key` instead, reached via the `self.palette.
        // is_some()` branch a few lines down: see that method's and the
        // `palette_input` field's own doc comments for the full mechanism
        // (gpui-component's `Input` already consumes everything else —
        // printable characters, caret movement, ctrl+a, ctrl+v — before a
        // raw `KeyDownEvent` would ever reach here at all).
        //
        // Converted once and reused below — the palette-toggle check and
        // the closed-palette dispatch both need it, and re-converting the
        // same raw event twice was pure waste.
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

        // Escape ends an in-flight drag of either kind before the matcher
        // ever sees the keystroke (post-merge review BUG 2) — the keyboard
        // is documented hot mid-drag, and Escape is the universal "abort
        // the transient thing" key everywhere else in the shell (modal
        // above, palette in `handle_palette_key`), so it belongs to the
        // drag while one is running. Slotted here deliberately: below the
        // modal/palette branches (while either overlay is open, Escape
        // keeps meaning "close the overlay" — any drag was already
        // cancelled when the overlay opened, save the one-frame window
        // the render-top guard closes at the next paint) and above
        // `matcher.press` (a drag-ending Escape must not also feed a
        // pending sequence or dispatch a binding). The two drag kinds end
        // per their own recorded semantics, and both cover the
        // armed-but-inactive state too:
        // - tile drag: CANCEL — nothing was applied, so ending it applies
        //   and persists nothing;
        // - divider drag: FINISH, not revert (recorded decision) — its
        //   resizes were applied live, cancel means "stop tracking the
        //   mouse", never "undo", so `cancel_divider_drag` keeps them and
        //   dirties the session exactly like a mouse-up finish would.
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
        match self.matcher.press(&self.services.keymap, keystroke, &stack) {
            MatchResult::Matched { action, count } => {
                self.dispatch(&action, count, window, cx);
                cx.notify();
            }
            MatchResult::Pending | MatchResult::NoMatch => {
                // The status bar shows pending keystrokes later (spec §3);
                // for now just repaint so nothing looks stuck.
                cx.notify();
            }
        }
    }
}
