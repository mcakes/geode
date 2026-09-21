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

/// A stack verb's refusal on a tile that is not a stack member
/// (tile-stacks spec §4) — `ShellView::notice`'s value for the rest of
/// that one dispatch.
pub(super) const NOT_IN_A_STACK: &str = "not in a stack";

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
        self.single_keystroke_binding(keystroke, &stack)
            .is_some_and(|binding| binding.action.0 == "palette::toggle")
    }

    /// The binding that governs `keystroke` as a *single* keystroke under
    /// `stack` — the last exact match whose predicate passes, mirroring
    /// `Matcher::press`'s last-exact-match-wins layering (spec §3.4) —
    /// or `None` when no binding claims it. Resolved straight against the
    /// keymap rather than through `self.matcher` so a caller with its own
    /// keyboard owner (the palette toggle above, the scope bar's text
    /// field in `handle_key_down`) never touches, or is confused by, the
    /// matcher's pending-sequence and count state: a count typed into a
    /// text field is text, and a chord typed there is not the second
    /// half of whatever sequence was pending before the field took focus.
    /// An unbind (`"ctrl+k" = "none"`) is a winner like any other: the
    /// caller reads `action` and treats [`UNBOUND_ACTION`] as "swallowed",
    /// the way the matcher does.
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

    /// Apply one resolved action id through the shell's one dispatch chain
    /// (spec: "one keymap, ours" — no parallel action-dispatch system).
    /// Workspace verbs go through `apply_workspace_action`; the shell's own
    /// non-workspace actions (`palette::toggle`, `settings::open`, …) are
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
        // Recorded before matching (Phase 4b Task 6): the crash hook's
        // only view into "what was the user doing" reads this tail
        // through the `Arc<Mutex<_>>` it was handed at startup, so every
        // dispatch — reached or not by the branches below — must land in
        // it first. A hash, not the `ActionId` itself (`ActionTail::
        // record`'s own doc comment): no allocation per keypress.
        self.services
            .action_tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&action.0);
        // The one line every dispatched action leaves in the daily log
        // (`[log] shell = "debug"`): which action, with what count. The
        // branch that resolved it says so on its own line just before.
        tracing::debug!(target: "geode::shell", action = %action.0, count = ?count, "dispatch");

        // A stack verb's refusal notice (tile-stacks spec §4) says its
        // piece for exactly one dispatch — the next one, whatever it is,
        // clears it. The transient member list (spec §5.2) is likewise
        // closed by any dispatch — the list's own keys never reach this
        // function (`handle_key_down`'s own branch, above, claims them
        // first), so this only ever fires for a keystroke or a mouse
        // action from OUTSIDE the list.
        self.notice = None;
        self.stack_list = None;

        if action.0 == "stack::next" || action.0 == "stack::prev" {
            // Tile stacks (spec §4): count-aware, so not in the router.
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

        // Every workspace verb below ignores the count; only the module
        // fall-through at the end (Phase 3 §3.3) is count-aware today.
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if handled {
            self.session_dirty = true;
            // Every workspace verb that can move which tile has focus
            // comes through here — the four directions, the workspace
            // switch, a dock show/toggle/move, a close — so this is the
            // one door I-3's rule needs (see
            // `note_keyboard_focus_move`). A verb that moves nothing but
            // geometry (a resize) reaches it too and costs one comparison:
            // narrowing the list by action id would be a second table of
            // verb names to keep in step with `apply_workspace_action`'s
            // own, which is exactly the kind of drift the mechanism rule
            // is against.
            self.note_keyboard_focus_move(window, cx);
        } else if action.0 == "palette::toggle" {
            self.toggle_palette(window, cx);
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
        } else if action.0 == "config::views" {
            // Phase 4c: the object dialog's browse stage over
            // `Domain::Views` (`shell::objectdialog`). Palette-only, like
            // `keybindings::open` above and for the same reason
            // (defaults.rs: no key binding).
            objectdialog::render::open(self, objectdialog::Domain::Views, window, cx);
        } else if action.0 == "config::groupings" {
            // Part 2a Task 4: the object dialog's browse stage over
            // `Domain::Groupings` (`shell::objectdialog::groupings`).
            // Palette-only, like `config::views` above.
            objectdialog::render::open(self, objectdialog::Domain::Groupings, window, cx);
        } else if action.0 == "config::scopes" {
            // Part 2a Task 5: the object dialog's browse stage over
            // `Domain::Scopes` (`shell::objectdialog::scopes`).
            // Palette-only, like `config::views` above.
            objectdialog::render::open(self, objectdialog::Domain::Scopes, window, cx);
        } else if action.0 == "config::schema" {
            // Part 2b Task 2: the object dialog's browse stage over
            // `Domain::Schema` (`shell::objectdialog::schema`), read-only.
            objectdialog::render::open(self, objectdialog::Domain::Schema, window, cx);
        } else if action.0 == "config::sources" {
            // Part 2b Task 3: the object dialog's browse stage over
            // `Domain::Sources` (`shell::objectdialog::sources`).
            // Palette-only, like `config::views` above.
            objectdialog::render::open(self, objectdialog::Domain::Sources, window, cx);
        } else if action.0 == "config::colours" {
            // Part 2c Task 5: the object dialog's browse stage over
            // `Domain::Colours` (`shell::objectdialog::colours`).
            // Palette-only, like `config::views` above.
            objectdialog::render::open(self, objectdialog::Domain::Colours, window, cx);
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
            // off → on → rel → off (user ruling 2026-09-11); the settings
            // row steps the same value, through the same setter.
            self.set_line_numbers(self.line_numbers.next(), cx);
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
        } else if action.0 == "scope::save_current" {
            // **Trap** (CLAUDE.md's Scopes bullet, `defaults.rs`'s own
            // registration comment): this arm MUST come before the
            // `strip_prefix("scope::")` one below, which would otherwise
            // read `save_current` as the name of a saved scope to load
            // rather than as this action's own id — the reason
            // `Domain::Scopes.reserved_names()` refuses a scope by that
            // name. Opens the Scopes dialog on the naming prompt, seeded
            // from the frame's current scope (scope-save spec's
            // amendment to Part 2a's `Domain::Scopes`) — same door the
            // scope bar's `save` chip uses.
            objectdialog::render::open_save_scope(self, window, cx);
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
        } else if action.0 == "frame::scope_expression" {
            // Palette-only (command-line locality spec §4.1): the typed door
            // onto the frame's expression layer — the same door the scope
            // bar's expression chip opens.
            scope_expr_view::open(self, window, cx);
        } else if action.0 == "frame::grouping" {
            // mod+g (2026-09-19): the grouping picker — the same door the
            // toolbar readout's click takes.
            choicedialog::open_grouping(self, window, cx);
        } else if action.0 == "tile::add" {
            // mod+n (2026-09-19): the tile picker — the same door a
            // placeholder's double-click takes
            // (`try_pick_tile_on_double_click`).
            choicedialog::open_tile_kinds(self, window, cx);
        } else if action.0 == "log::level" {
            // Palette-only (command-line locality spec §4.2): the two-step
            // log-level picker, `:level`'s replacement.
            choicedialog::open_log_level(self, window, cx);
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
        } else if let Some((kind, placement)) = crate::defaults::parse_add_action(&action.0) {
            // A palette row from `register_add_actions` (spec 2026-09-08
            // add-tile §3.2) — "<Kind>: Split" follows the setting; the
            // suffixed rows say where, `_stacked` onto the focused tile
            // (tile-stacks spec §6.1). Always adds (or fills); never
            // focuses an existing tile — that is `open_module`'s job.
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
    /// after applying a change live through `ThemeService`
    /// (`dispatch_palette_item`'s `Theme` branch below and
    /// `settings_view::set_theme`) — one place that
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
    /// `user_dir`/`active_name` off `self` — happens here, on
    /// the UI thread; the actual read-modify-write runs inside a task handed
    /// to `cx.background_executor()`, fire-and-forget (`.detach()`): theme
    /// changes are infrequent (a user action, not a hot path like key-repeat),
    /// so there's no need for the session-save path's coalescing
    /// dirty-flag/watcher-tick machinery here — a plain spawn per change is
    /// simple and cheap enough. A write failure surfaces as a `geode::theme`
    /// warning from inside the task, never a crash — same convention as
    /// every other config-write failure in this codebase.
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
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = theme::persist_to_user_config(&dir, &name) {
                    tracing::warn!(target: "geode::theme", "{e}");
                }
            })
            .detach();
    }

    /// Persist the current font size to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_theme`] just above (missing `user_dir` = silently skipped;
    /// failures are a `geode::config` warning; last-write-wins races
    /// accepted for the same rare-UI-action reasons).
    pub(super) fn persist_font_size(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let size = self.font_size;
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = fontsize::persist_to_user_config(&dir, size) {
                    // MIN-2: `fontsize`/`vimfind`/`theme::persist_to_
                    // user_config` all write the same `app.toml` and, on
                    // a parse failure, return byte-identical text — a
                    // bare `{e}` here and in `persist_find_style` below
                    // would be indistinguishable at `geode::config`
                    // (`persist_theme`'s own failure already reads
                    // apart, since it logs at the `geode::theme` target
                    // instead). The leading phrase is the only thing
                    // that tells the two apart.
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
    /// skipped; failures are a `geode::config` warning; last-write-wins
    /// races accepted for the same rare-UI-action reasons).
    pub(super) fn persist_find_style(&self, cx: &mut Context<Self>) {
        let Some(dir) = self.user_dir.clone() else {
            return;
        };
        let style = self.find_style;
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = vimfind::persist_to_user_config(&dir, style) {
                    // MIN-2 — see `persist_font_size`'s comment just above.
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
        cx.background_executor()
            .spawn(async move {
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
        self.fetch_sources = series.names();
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
        cx.background_executor()
            .spawn(async move {
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
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = crate::tileadd::persist_to_user_config(&dir, direction) {
                    tracing::warn!(target: "geode::config", "add direction not saved: {e}");
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
        // Task 9 instant-modal redesign (see `dialog`'s module doc): while
        // Geode's own modal (`self.modal`) OR a gpui-component `Dialog`
        // layer is open, the shell's own keymap `Matcher` must not see a
        // single keystroke — otherwise e.g. `ctrl+w` typed inside the
        // settings dialog would *also* dispatch `workspace::close_tile`
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
        // `listening` branch) and every modal dialog's escape ladder
        // (`dialogmode::escape_step`), whose last rung is the one
        // `escape` that reaches this branch's close.
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
                // The key-path seam of `dialog::sync_dialog_text`'s four
                // seam classes (spec §16.1/§16.6): the handler above is
                // a pure mutation of the dialog's own `mode`/`query`,
                // and this is what makes gpui agree with it. Claimed or
                // not: the rule is "reconcile after the handler", never
                // "after a claim" — an unclaimed key that had moved the
                // pure state would otherwise leave focus and the field
                // behind it, and whether a given key claims is not this
                // seam's business. Guarded by `self.modal.is_some()`
                // because the unclaimed-`escape` branch below closes the
                // modal, and `close_modal` owns focus once the dialog is
                // gone; and placed ahead of both exits so neither path
                // can skip it.
                if self.modal.is_some() {
                    dialog::sync_dialog_text(self, window, cx);
                }
                if handled {
                    // A key the modal claimed must not also reach the
                    // window's text-input phase. That phase is what a
                    // focused `Input` actually inserts characters from
                    // (`Window::dispatch_keystroke`: it runs only when the
                    // key event still `propagate`s after every listener),
                    // and the dialogs' shared filter field is focused
                    // whenever a list dialog is in filter mode (or is one
                    // of the filter-only dialogs) — so without this,
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
        // focus — typing must reach it, not the shell's keymap `Matcher`.
        // One of TWO such rules now: the insert-mode branch just below
        // generalises this same shape to a tile that owns a focused
        // `Input` of its own (market-data spec §8.6, the panel's cell
        // editor). They are deliberately separate branches rather than one
        // merged guard — this one resolves chords against the `workspace`
        // context alone and reflects the frame's text back into the field
        // afterwards, neither of which is true of a tile's own input.
        // This has to be handled explicitly rather than relying on gpui's
        // dispatch to simply not reach here: gpui-component's `Input`
        // binds most editing keys (typing, backspace, arrows, ctrl+v
        // paste, …) as *actions* scoped to its own key context, but its
        // `Escape` action handler calls `cx.propagate()` whenever there
        // is no popover/inline-completion/IME-marked-text/`clean_on_escape`
        // to consume it (the plain-filter case, always, here) — and any key
        // with *no* action binding at all in that context (e.g. `ctrl+w`,
        // `ctrl+k`, bare typed letters) skips the action system entirely.
        // Both cases still deliver the raw `KeyDownEvent` to every
        // `on_key_down` listener up the dispatch path, this one included
        // (verified against the pinned gpui rev's `Window::
        // finish_dispatch_key_event`/`dispatch_key_down_up_event`), so
        // without this guard a bare `j` typed into the filter would *also*
        // reach the matcher as a blotter motion.
        //
        // Two kinds of key still act on the shell from here. Esc hands
        // focus back to the shell root so hjkl and friends resume working
        // immediately. And a *chord* — any keystroke carrying ctrl, alt or
        // cmd (`Modifiers::is_chord`; shift alone is typing, `shift+d` is
        // `D`) — dispatches its shell binding (user ruling 2026-09-12: the
        // original brief's "shell chords won't fire — acceptable while
        // typing a filter" is superseded; `ctrl+k`, `ctrl+,` and the rest
        // must work from the field). Resolved as a single keystroke
        // against the `workspace` context ALONE, never `context_stack`:
        // while the field has focus the keyboard belongs to the field and
        // the shell's chrome, not to whichever tile the layout has
        // focused, so a blotter's `ctrl+d` (page down) does not page the
        // blotter behind a trader's typing. Only chords the `Input` left
        // alone ever arrive here (it consumes its own — cmd+a, cmd+v,
        // shift+arrows — as actions before any listener runs), so a
        // dispatched chord is one the field had no use for; it is still
        // stopped from propagating, since a macOS `alt+letter` carries a
        // typed character the field would otherwise insert. A dispatched
        // action may move the frame's text (`mod+z` undoes the session's
        // own typing) while the field stays focused — the field is then
        // reflected from the frame here, the one place the "unfocused
        // field shows the frame's truth" rule of `on_frame_changed` is
        // applied to a focused one, because after an action the action's
        // result is the truth, not the caret. `set_value` emits no
        // `Change`, so the reflection cannot feed back into the session.
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

        // Insert mode (market-data spec §8.6): a tile occupant that owns a
        // focused `Input` — the market-data panel's cell editor — reports
        // `mode == insert` in its key context. While it does, and window
        // focus is on a handle the shell does not own, only SINGLE-
        // keystroke bindings resolve against the context stack (the module
        // fragment's own `escape`/`enter` in `<kind> && mode == insert`,
        // and chords); every other keystroke propagates untouched to the
        // focused input, and the matcher's sequence and count state is
        // never fed — the same shape as the filter-field branch above,
        // generalised to a tile.
        //
        // This exists because this listener sits on the window root and
        // sees every raw keystroke, focused element or not (the filter
        // branch's own comment has the gpui mechanics): without it a typed
        // `j` would also move the panel's cursor, and a typed `5` would
        // leave the matcher holding a count of 5 to multiply whatever
        // motion the trader made after leaving the cell.
        //
        // Which contexts a keystroke resolves against splits on whether it
        // is a CHORD — the same `Modifiers::is_chord` line the filter field
        // draws (ctrl, alt or cmd; shift alone is typing, `shift+d` is `D`)
        // — because the two kinds of key mean opposite things here
        // (controller ruling):
        //
        // * A chord resolves against the WHOLE stack, unlike the filter
        //   field's `workspace`-only resolution: the keyboard belongs to
        //   the tile, so the tile's own chords are exactly the ones that
        //   should win, and a shipped shell chord (`ctrl+k`) is a promise
        //   that holds while typing.
        // * A BARE key resolves only against the contexts that themselves
        //   carry `mode == insert` — in practice the tile's own, the rest
        //   of the stack filtered out. Every bare key the shell binds is a
        //   character a trader types into a cell: `/` and `:` open the find
        //   and command lines from the `tile` context, `shift+d` duplicates
        //   the tile from `workspace`. None of them may fire behind typing,
        //   and making every future text-entry module reclaim each one in
        //   its own fragment is the wrong side of this seam — the shell
        //   knows it is in insert mode, so the shell answers for it.
        //
        // A module's own insert-mode bindings still resolve either way,
        // because the context they name (`<kind> && mode == insert`, in a
        // keymap fragment — `keymap::fragments`, spliced above the
        // compiled-in layers) is precisely the one that is kept: that is
        // what makes `escape` cancel and `enter` commit while every other
        // bare key is text.
        //
        // The focus test comes FIRST, ahead of building the stack: focus is
        // on the shell's own root for all but a vanishing minority of
        // keystrokes, so the cheap comparison short-circuits before the
        // stack's allocation on the ordinary path. The two conditions are a
        // conjunction either way — an insert-mode context is meaningless
        // while the shell itself holds the keyboard (a modal, the palette,
        // the command line and the scope bar all returned above, but a
        // shell surface focused with none of them OPEN would otherwise
        // route its typing at a tile).
        //
        // On commit or cancel the occupant gives the keyboard up and drops
        // its `InputState`, and `render`'s `window.focused(cx).is_none()`
        // net is what turns that into shell focus — no module touches the
        // shell's own focus handle (CLAUDE.md's focus rule). Giving it up
        // takes a `Window::blur`, not just the drop: gpui-component's
        // `Root` holds the focused input as a strong `AnyInputState` and
        // unregisters it only from that input's own render, which an input
        // removed from the tree never reaches (`module::recording`'s
        // commit/cancel arm has the full note; a real panel owes the same
        // two steps).
        //
        // `occupant_insert_stack` (occupants.rs) is the ONE predicate for
        // "the focused tile's occupant itself holds the keyboard in insert
        // mode" — ownership included (`TileContent::holds_focus`), so a
        // keystroke with the ring on one tile and the keyboard in
        // another's abandoned field never resolves against the first's
        // insert stack. `render`'s focus-restore skip reads the same door
        // (user ruling 2026-09-17), so the two can never disagree about
        // whose keyboard it is.
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

        if let Some(list) = self.stack_list.clone() {
            // A CHORD (ctrl/alt/cmd — `Modifiers::is_chord`, the same line
            // the filter-field branch above draws) is not a list key: a
            // shipped shell chord (`ctrl+k` closes the list itself, via
            // `toggle_palette`, before this branch even runs — see its own
            // comment) must still fire from inside the list exactly as it
            // does from inside a text field, and something like `ctrl+3`
            // (a grouping slot) must not be read as "activate member 3".
            // Deliberately NOT returning here: the keystroke falls through
            // to the matcher below, whose `dispatch` clears `stack_list`
            // at its own top (fix round 1, Ruling 5).
            let is_chord = convert_keystroke(&event.keystroke).is_some_and(|ks| ks.mods.is_chord());
            if !is_chord {
                // The member list owns the keyboard while open (spec
                // §5.2): `j`/`k`/arrows step with wrap, a digit activates
                // at once, `enter` activates the highlighted row, `escape`
                // closes with no change. Every other bare key is swallowed
                // here too — the list is modal in the same sense the
                // palette is, and the matcher must not see a keystroke
                // behind it.
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
                // The status bar shows pending keystrokes later (spec §3);
                // for now just repaint so nothing looks stuck.
                cx.notify();
            }
        }
    }
}

/// The contexts `keystroke` resolves against inside `handle_key_down`'s
/// insert-mode branch: the whole `stack` for a chord, and only the
/// contexts carrying `mode == insert` for a bare key (controller ruling,
/// market-data spec §8.6 — see the branch's own comment for why the two
/// differ). A free function rather than a method: it reads nothing but its
/// arguments, which is also what lets it be tested without a window.
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
