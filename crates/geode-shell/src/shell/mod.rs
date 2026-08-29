//! The shell's window root view (spec §3): a single view owning the whole
//! window contents, key dispatch, and workspace state. Chrome (Task 4):
//! `toolbar::toolbar` (the native title bar) on top, `sidebar::sidebar`
//! (workspace indicators + profile icon) on the left, `status::status_bar`
//! (pending keys, reload indicator, theme name) on the bottom. Between
//! them, the tiling tree (Task 3) renders as themed, absolutely-positioned
//! tiles over whatever rect is left. Task 6 wires the real command palette.

pub mod dialog;
pub mod keybindings_view;
pub mod keys;
pub mod settings_view;
pub mod sidebar;
pub mod status;
pub mod toolbar;
pub mod whichkey;

pub use keys::convert_keystroke;

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, MouseButton, ScrollHandle, Window,
    div, px,
};
use gpui_component::input::InputState;
use gpui_component::{ActiveTheme as _, Root, TITLE_BAR_HEIGHT, WindowExt as _, h_flex, v_flex};

use crate::actions::{ActionId, ActionRegistry};
use crate::defaults::mod_alias_from_config;
use crate::fonts;
use crate::fontsize::{self, FontSize};
use crate::keymap::{KeyContext, Keymap, MatchResult, Matcher, Modifiers, build_keymap};
use crate::palette::{self, PaletteItem, PaletteState};
use crate::reload;
use crate::session;
use crate::theme;
use crate::theme::ThemeService;
use crate::tiling::{Rect, Workspaces, apply_workspace_action};
use geode_core::config::{Config, LayerDoc};

/// How often the background reload watcher polls the watched config
/// directories' `*.toml` mtimes (brief: "~500ms"). File scanning and
/// `Config::load` themselves run off the UI thread (`cx.background_executor
/// ().spawn`); only the cheap decision + entity mutation happens on the UI
/// thread, via the async entity handle (spec PHILOSOPHY.md: "nothing may
/// stall the render thread").
const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Everything the shell needs to run a window, assembled once by the app
/// from loaded config, the action registry, the compiled keymap, and the
/// initial workspace state (spec §3, §8). `ShellView` owns this for the
/// life of the window.
pub struct ShellServices {
    pub config: Config,
    pub registry: ActionRegistry,
    pub keymap: Keymap,
    pub mod_alias: Modifiers,
    pub workspaces: Workspaces,
    pub theme: ThemeService,
    /// Where `ShellView::dispatch` saves the session file after a
    /// workspace-mutating action (Task 3, spec: "state-as-config"). `None`
    /// in contexts with no writable user config dir (e.g. some test setups)
    /// — session persistence is then just skipped, never a panic.
    pub session_path: Option<PathBuf>,
}

/// The window's root view. Intercepts all keyboard input via `on_key_down`
/// rather than gpui's own action-dispatch system, because key resolution
/// here goes through the shell's own layered, sequence-aware [`Matcher`]
/// (spec §3.4), not a static `KeyBinding` table.
pub struct ShellView {
    services: ShellServices,
    matcher: Matcher,
    /// The effective UI font size (small/medium/large — `[ui] font_size`).
    /// Applied as the window's rem size at the top of `render`, the one
    /// place with a `Window` on every path that can change it (startup,
    /// the settings control via `settings_view::set_font_size`, config hot
    /// reload) — see the `fontsize` module doc.
    font_size: FontSize,
    focus_handle: FocusHandle,
    /// The open command palette's state (Task 6), or `None` when closed.
    /// Built fresh from the registry/keymap/theme service each time
    /// `palette::toggle` opens it (brief: the reverse binding index is
    /// built once at palette-open, not per frame) and dropped on close —
    /// nothing about it survives being closed and reopened.
    palette: Option<PaletteState>,
    /// The open modal's state (Task 9, instant-modal redesign), or `None`
    /// when closed. Set only through [`dialog::open_shell_dialog`] (the one
    /// standard door — see that function's and `dialog`'s module doc), read
    /// by `Render for ShellView` to paint the backdrop/panel/title-row/
    /// close-button chrome (`dialog::render_modal`) and by
    /// [`handle_key_down`](Self::handle_key_down)'s modal branch, which
    /// makes Escape close it and swallows every other shell chord while
    /// it's open. Unlike `palette`, nothing here is per-frame scroll state
    /// to track alongside it — a modal's content owns whatever internal
    /// state it needs (e.g. the settings composite's own search input).
    modal: Option<dialog::ShellModal>,
    /// The open keybinding dialog's own pure state (Part B), or `None` when
    /// closed/never opened. Set fresh by [`keybindings_view::open`] each
    /// time (mirrors `palette`'s "nothing survives a close/reopen"
    /// contract) and read/mutated both by the modal's render closure
    /// (`keybindings_view::build`, via a plain `&ShellView` reborrow — see
    /// `ShellModal::build`'s doc comment) and by its
    /// [`dialog::ModalKeyHandler`] (`keybindings_view::handle_key`, reached
    /// through `handle_key_down`'s modal branch). Deliberately holds no
    /// `gpui` types itself (`vimnav::VimListNav`, a selection index, and the
    /// in-progress capture sequence only) so it stays unit-testable without
    /// a window, the same way `PaletteState` does — the dialog's
    /// `ScrollHandle` lives in the sibling `keybindings_scroll` field below
    /// instead, following `palette`/`palette_scroll`'s own split exactly.
    keybindings: Option<keybindings_view::KeybindingsState>,
    /// Scroll state for the open keybinding dialog's row list — same
    /// reasoning and lifecycle as `palette_scroll` (a fresh `ScrollHandle`
    /// per open, driven by `keybindings_view`'s selection-change paths via
    /// `ScrollHandle::scroll_to_item`).
    keybindings_scroll: ScrollHandle,
    /// Scroll state for the open palette's results list, tracked across
    /// frames the same way `filter_input`'s `Entity<InputState>` is
    /// (`gpui::ScrollHandle` is a cheap `Clone` — `Rc<RefCell<..>>` — but a
    /// *fresh* one must still be handed to `track_scroll` every frame the
    /// list renders, so this is that stable handle). Rebuilt alongside
    /// `palette` in `toggle_palette` on every open, and driven from the
    /// same selection-change path as `palette` itself (`move_selection`,
    /// `push_char`, `backspace` in `handle_palette_key`) via `sync_palette_
    /// scroll`, so the selected row always scrolls into view.
    palette_scroll: ScrollHandle,
    /// Desk and user config directories the reload watcher polls (Task
    /// 1c-1). Owned here (not just captured by the background task) so the
    /// watcher's own loop re-reads them fresh from the entity each poll —
    /// a single source of truth, rather than a stale copy baked in at
    /// spawn time.
    desk_dir: Option<PathBuf>,
    user_dir: Option<PathBuf>,
    /// The mtime snapshot as of the last poll. Starts as `Snapshot::
    /// default()` (empty) — `reload::scan` does real filesystem I/O, so it
    /// must not run synchronously in `new` on the UI thread (spec
    /// PHILOSOPHY.md: "nothing may stall the render thread"); the watcher's
    /// first poll iteration performs the real seed scan on the background
    /// executor instead, storing it here *without* treating it as a
    /// "change" (see `new`'s doc comment on why: comparing it against the
    /// empty default would always look changed and trigger a spurious
    /// reload on every window open). Compared against a fresh scan every
    /// ~500ms after that; a difference is what triggers loading a new
    /// `Config`.
    last_snapshot: reload::Snapshot,
    /// The result of the last reload attempt, `Unchanged` until the first
    /// one runs. Drives the status bar's reload indicator.
    last_reload: reload::ReloadOutcome,
    /// Set on every successful workspace-mutating `dispatch`; cleared by
    /// the background watcher's ~500ms tick (see `new`), which is also
    /// where the actual file write happens — off the UI thread (Task 3 fix
    /// round 1: the first cut of this wrote synchronously per dispatch,
    /// which stalls the render thread on a slow filesystem under OS
    /// key-repeat, e.g. holding shift+left at ~20-30 events/sec). Coalescing
    /// onto the existing poll tick means at most one write per ~500ms
    /// regardless of how many workspace actions fired in that window.
    session_dirty: bool,
    /// The toolbar's right-aligned filter field (Task 4). Deliberately
    /// inert — nothing reads its value; it becomes the global text filter
    /// (spec §4.1) in the data phase. Owned here (rather than built fresh
    /// per render, like `status_bar`/`sidebar`'s stateless element fns) is
    /// required: `Input` is a stateful gpui-component that needs a stable
    /// `Entity<InputState>` across frames to keep its own cursor/selection/
    /// focus state, not something rebuildable from scratch each render.
    filter_input: Entity<InputState>,
}

/// Whether two layered keymap-doc slices (`Config::layered_docs("keymap")`,
/// Builtin → Desk → User order) are identical — content, not just count.
/// Used by [`ShellView::apply_reload`] (Review fix round 1, Finding 2) to
/// decide whether a reload's palette-relevant inputs actually changed.
/// A free function comparing fields directly rather than a `PartialEq`
/// derive on `LayerDoc` itself (`geode_core::config`): every field here
/// already implements `PartialEq` (`Layer`, `String`, `PathBuf`,
/// `toml::Table`), so this needs no change to that shared type just for
/// one call site.
fn keymap_docs_equal(a: &[LayerDoc], b: &[LayerDoc]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.layer == y.layer && x.name == y.name && x.file == y.file && x.table == y.table
        })
}

impl ShellView {
    pub fn new(
        services: ShellServices,
        desk_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        // The toolbar's filter field (Task 4): built once here, not per
        // render, so `Input`'s own cursor/selection/focus state survives
        // across frames.
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("filter"));

        // `last_snapshot` starts empty rather than being seeded with a
        // synchronous `reload::scan` call right here: that would be real
        // filesystem I/O on the UI thread, during `new` (spec PHILOSOPHY.md
        // — review finding: the seed scan is exactly as much "the UI
        // thread" as any other poll). The watcher spawned below performs
        // the real seed scan, off-thread, as its first iteration.
        cx.spawn(async move |this, cx| {
            let mut is_first_poll = true;
            loop {
                cx.background_executor().timer(RELOAD_POLL_INTERVAL).await;

                // Flush a dirty session (Task 3 fix round 1), coalesced
                // onto this same ~500ms tick rather than writing per
                // dispatch. `take_dirty_session_write` does the cheap part
                // (TOML serialization) synchronously on the UI thread via
                // `this.update`; the actual file write — real, potentially
                // blocking I/O — runs on the background executor, so it
                // can never stall the render thread no matter how slow the
                // filesystem is. Runs unconditionally on every tick, ahead
                // of the `continue`s below, so it's never skipped by the
                // reload watcher's own early-outs.
                let Ok(pending_write) =
                    this.update(cx, |view, _cx| view.take_dirty_session_write())
                else {
                    return; // window/entity gone; stop polling
                };
                if let Some((path, text)) = pending_write {
                    cx.background_executor()
                        .spawn(async move { session::write_atomic(&path, &text) })
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("[session] warning: failed to save session: {e}")
                        });
                }

                let Ok((desk_dir, user_dir)) = this.update(cx, |view, _cx| {
                    (view.desk_dir.clone(), view.user_dir.clone())
                }) else {
                    return; // window/entity gone; stop polling
                };

                let scan_desk = desk_dir.clone();
                let scan_user = user_dir.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { reload::scan(scan_desk.as_deref(), scan_user.as_deref()) })
                    .await;

                if is_first_poll {
                    is_first_poll = false;
                    // Seed-only: store this first background-thread scan as
                    // the baseline and move on, without comparing it to the
                    // `Snapshot::default()` placeholder — that comparison
                    // would always read as "changed" (default is empty,
                    // and a real desk/user dir practically never is) and
                    // trigger a reload of the config `new` was just handed,
                    // on every window open.
                    if this
                        .update(cx, |view, _cx| view.last_snapshot = snapshot)
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }

                let Ok(changed) = this.update(cx, |view, _cx| {
                    let changed = snapshot.changed_since(&view.last_snapshot);
                    if changed {
                        view.last_snapshot = snapshot.clone();
                    }
                    changed
                }) else {
                    return;
                };
                if !changed {
                    continue;
                }

                let new_config = cx
                    .background_executor()
                    .spawn(async move { reload::load_config(desk_dir, user_dir) })
                    .await;

                if this
                    .update(cx, |view, cx| view.apply_reload(new_config, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();

        let font_size = FontSize::from_config(&services.config);

        Self {
            services,
            matcher: Matcher::default(),
            font_size,
            focus_handle,
            palette: None,
            modal: None,
            keybindings: None,
            keybindings_scroll: ScrollHandle::new(),
            palette_scroll: ScrollHandle::new(),
            desk_dir,
            user_dir,
            last_snapshot: reload::Snapshot::default(),
            last_reload: reload::ReloadOutcome::Unchanged,
            session_dirty: false,
            filter_input,
        }
    }

    /// Apply (or reject) a freshly loaded `Config` (Task 1c-1): rebuild the
    /// keymap and mod alias from it, re-apply the theme only if `[theme]`
    /// actually changed (so a runtime `theme::toggle_mode` isn't silently
    /// clobbered by an unrelated reload — e.g. only `keymap.toml` edited),
    /// close an open palette only if ITS snapshot inputs actually changed
    /// (Review fix round 1, Finding 2 — refines the original brief's "must
    /// close on a successful reload": a theme-only reload no longer closes
    /// it, since the palette's items don't depend on `[theme]` at all; see
    /// `keymap_docs_equal` and the `palette_snapshot_changed` check below),
    /// and record the outcome for the status bar.
    ///
    /// Any error-severity diagnostic — from `Config::load` itself, or from
    /// building the keymap against the new config's docs — keeps the
    /// entire previous `Config` (and everything built from it) untouched
    /// (plan constraint: "Invalid config never panics: any error
    /// diagnostic ⇒ keep last-good entire Config"). Called by the
    /// background watcher above, and directly by tests: gpui's test
    /// executor never advances its simulated clock on `run_until_parked`
    /// (confirmed against the pinned rev's `TestScheduler::run`), so there
    /// is no practical way to drive the watcher's own timer loop through a
    /// `#[gpui::test]`; this is the real apply path either way; the
    /// watcher is just what schedules calling it.
    fn apply_reload(&mut self, mut new_config: Config, cx: &mut Context<Self>) {
        let mod_alias = mod_alias_from_config(&new_config);
        let (keymap, keymap_diags) = build_keymap(
            new_config.layered_docs("keymap"),
            mod_alias,
            &self.services.registry,
        );
        new_config.diagnostics.extend(keymap_diags);

        let outcome = reload::decide(&new_config);
        if let reload::ReloadOutcome::Applied { warnings } = &outcome {
            // Fix wave, Fix 4: `decide` folds warning-severity diagnostics
            // (config + keymap-build) into `Applied { warnings }` rather
            // than discarding them, but nothing previously read that field
            // — a warning-only reload (e.g. an unknown-but-non-fatal
            // keymap key) applied silently with no trace anywhere. Surface
            // each on stderr, one line per warning, the same
            // `[source] warning: message` convention `main.rs`'s startup
            // diagnostics already use (these are plain `String`s by the
            // time they reach here — `decide` already extracted
            // `Diagnostic::message` — so there's no `Diagnostic` Display
            // impl to reuse here).
            for warning in warnings {
                eprintln!("[reload] warning: {warning}");
            }

            let theme_changed =
                self.services.config.get("app", "theme") != new_config.get("app", "theme");
            // Review fix round 1, Finding 2: only close the palette when
            // its own snapshot inputs (Task 6: bindings, built in
            // `toggle_palette` from the raw keymap docs + the resolved mod
            // alias) could actually have changed — a theme-only reload
            // (including the one our own `theme::persist_to_user_config`
            // write triggers, see that function's doc comment) must not
            // silently close an open palette out from under the user.
            let palette_snapshot_changed = self.services.mod_alias != mod_alias
                || !keymap_docs_equal(
                    self.services.config.layered_docs("keymap"),
                    new_config.layered_docs("keymap"),
                );

            self.services.config = new_config;
            self.services.mod_alias = mod_alias;
            self.services.keymap = keymap;
            // Cheap re-derive; `render` applies it only when it changed.
            self.font_size = FontSize::from_config(&self.services.config);

            if theme_changed {
                self.services
                    .theme
                    .apply_from_config(&self.services.config, cx);
            }

            if palette_snapshot_changed {
                self.palette = None;
            }
        }

        self.last_reload = outcome;
        cx.notify();
    }

    /// The active context stack for key resolution, outermost first:
    /// `workspace` is always active; `palette` layers on top while open.
    /// Currently only consulted by [`is_palette_toggle`](Self::is_palette_toggle)
    /// (to gate that binding's own `context`, if a user keymap ever adds
    /// one) — `handle_key_down` never reaches `self.matcher.press` while
    /// `self.palette` is `Some`, since palette-open key handling is
    /// exclusive (see that method's doc comment).
    fn context_stack(&self) -> Vec<KeyContext> {
        let mut stack = vec![KeyContext::new("workspace")];
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
    fn is_palette_toggle(&self, keystroke: &crate::keymap::Keystroke) -> bool {
        let stack = self.context_stack();
        let winner = self.services.keymap.bindings().iter().rfind(|binding| {
            binding.keystrokes.len() == 1
                && binding.keystrokes[0] == *keystroke
                && binding.predicate.as_ref().is_none_or(|p| p.eval(&stack))
        });
        winner.is_some_and(|binding| binding.action.0 == "palette::toggle")
    }

    /// Open the palette (building a fresh `PaletteState` — actions in
    /// registry order, then themes) if it's closed, or close it if it's
    /// open.
    fn toggle_palette(&mut self) {
        if self.palette.is_some() {
            self.palette = None;
            return;
        }
        // Opening the palette cancels any pending keymap sequence (spec:
        // palette-open cancels pending — supersedes the 1b-ui deferred
        // note that pending state would survive a palette session). The
        // palette has its own key handling (`handle_palette_key`) that
        // never touches `self.matcher`, so without this an unfinished
        // sequence like the first "g" of "g g" would sit in `self.matcher`
        // across the whole palette session and then resume matching
        // against whatever key closes the palette.
        self.matcher.cancel();
        let bindings = palette::build_binding_index(&self.services.keymap);
        let items = palette::build_items(&self.services.registry, &self.services.theme, &bindings);
        self.palette = Some(PaletteState::new(items));
        // Fresh scroll state for a fresh palette session — a stale offset
        // left over from a previous open (a different query, a different
        // scroll position) must not carry over now that the results list
        // scrolls a real viewport instead of always fitting on screen.
        self.palette_scroll = ScrollHandle::new();
    }

    /// Scroll the palette's results viewport so the currently selected row
    /// is visible (`gpui::ScrollHandle::scroll_to_item`, a real per-frame
    /// layout measurement — see `palette::render`'s doc comment). Called
    /// from every path in `handle_palette_key` that can change `self.
    /// palette`'s selection: the two arrow/ctrl+p/ctrl+n branches, and the
    /// query-edit branches (`push_char`/`backspace` both reset the
    /// selection to row 0, which is itself a selection change the viewport
    /// must follow). A no-op while the palette is closed.
    fn sync_palette_scroll(&self) {
        if let Some(palette) = self.palette.as_ref() {
            self.palette_scroll.scroll_to_item(palette.selected());
        }
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
    fn dispatch(&mut self, action: &ActionId, window: &mut Window, cx: &mut Context<Self>) {
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if handled {
            self.session_dirty = true;
        } else if action.0 == "palette::toggle" {
            self.toggle_palette();
        } else if action.0 == "theme::toggle_mode" {
            self.services.theme.toggle_mode(cx);
            self.persist_theme(cx);
        } else if action.0 == "settings::open" {
            // Task 5: the real settings dialog (gpui-component's `setting`
            // module, wrapped in Geode's own instant modal chrome — Task 9
            // redesign, see `dialog`'s module doc). Reachable via `ctrl+,`,
            // the palette, and the sidebar profile icon. Goes through
            // `dialog::open_shell_dialog` (Task 9) via `settings_view::open`
            // itself, so it gets the crate's uniform open-time hygiene.
            settings_view::open(self, window, cx);
        } else if action.0 == "keybindings::open" {
            // Part B: the keybinding dialog itself (vimnav.rs +
            // keymap_edit.rs are its pure cores). Reachable today only via
            // the palette (defaults.rs: no key binding).
            keybindings_view::open(self, window, cx);
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
    fn persist_theme(&self, cx: &mut Context<Self>) {
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
    fn persist_font_size(&self, cx: &mut Context<Self>) {
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

    /// If a workspace mutation happened since the last flush, serialize the
    /// current session state (cheap: `session::to_string_pretty` over a
    /// handful of small TOML tables — safe to run synchronously here, on
    /// the UI thread, unlike the actual file write) and clear the dirty
    /// flag, handing the caller `(path, text)` to write off the UI thread.
    /// Returns `None` when there's nothing to flush (not dirty, no session
    /// path configured, or serialization somehow failed — logged as a
    /// warning either way, never a panic).
    ///
    /// Called from the background watcher's ~500ms tick (`new`) in
    /// production. Tests call it directly instead of driving that timer:
    /// gpui's test executor never advances its simulated clock under
    /// `run_until_parked` (same reasoning as `apply_reload`'s doc comment),
    /// so there's no practical way to wait out a real ~500ms poll in a
    /// `#[gpui::test]` — this is the real flush logic either way; the
    /// watcher loop is just what schedules calling it.
    fn take_dirty_session_write(&mut self) -> Option<(PathBuf, String)> {
        if !self.session_dirty {
            return None;
        }
        self.session_dirty = false;
        let path = self.services.session_path.clone()?;
        match session::to_string_pretty(&self.services.workspaces) {
            Ok(text) => Some((path, text)),
            Err(e) => {
                eprintln!("[session] warning: failed to serialize session: {e}");
                None
            }
        }
    }

    /// Write the current workspace layout to the session file, if one is
    /// configured (`ShellServices::session_path`), synchronously and
    /// unconditionally (ignores `session_dirty` — this is the "flush no
    /// matter what" path, not the coalesced per-dispatch one). The only
    /// caller is `main.rs`'s best-effort `on_app_quit` hook: a one-shot at
    /// shutdown, not a per-keystroke hot path, so a synchronous atomic write
    /// (`session::save`) here is fine — it does not reintroduce the
    /// render-thread stall Task 3 fix round 1 removed from `dispatch`. A
    /// write failure (e.g. an unwritable directory) is a warning line,
    /// never a panic — session persistence is a convenience, not a
    /// correctness requirement (mirrors config's own "bad input is a
    /// warning" philosophy).
    pub fn save_session(&self) {
        let Some(path) = self.services.session_path.as_ref() else {
            return;
        };
        if let Err(e) = session::save(path, &self.services.workspaces) {
            eprintln!("[session] warning: failed to save session: {e}");
        }
    }

    /// Dispatch one selected palette row: an `Action` item goes through the
    /// normal [`dispatch`](Self::dispatch) chain (brief: "action -> the
    /// normal dispatch chain incl. theme::toggle_mode"); a `Theme` item
    /// applies that theme directly via `ThemeService::apply`. The palette
    /// is assumed already closed by the caller (Enter closes before
    /// dispatching) — so the `palette::toggle` action id is deliberately
    /// *not* re-dispatched here: `dispatch`'s `palette::toggle` branch
    /// calls `toggle_palette`, which would reopen the just-closed palette,
    /// turning "select 'Toggle command palette'" into "close then
    /// immediately reopen". Skipping it instead makes selecting that row a
    /// true toggle: the palette just closes and stays closed, exactly like
    /// pressing the toggle keystroke a second time would.
    fn dispatch_palette_item(
        &mut self,
        item: &PaletteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match item {
            PaletteItem::Action(id, ..) if id.0 == "palette::toggle" => {}
            PaletteItem::Action(id, ..) => self.dispatch(id, window, cx),
            PaletteItem::Theme(name) => {
                // The name is already fully qualified (e.g. "Gruvbox
                // Dark"), which `ThemeService::resolve` matches outright
                // regardless of the `mode` argument — so the mode passed
                // here is irrelevant to which theme gets applied.
                self.services
                    .theme
                    .apply(name, crate::theme::Mode::Dark, cx);
                self.persist_theme(cx);
            }
        }
    }

    /// Handle one key event while the palette is open. Exclusive routing
    /// (plan constraint: "keyboard-first ... open, type, navigate,
    /// dispatch, close"; brief: "Palette-open swallows all other bindings
    /// ... keys go to the palette handler exclusively") — the shell's own
    /// keymap `Matcher` is never consulted here, so no other binding (a
    /// sequence, a workspace verb, anything) can leak through while typing
    /// a query. The palette-toggle keystroke itself is intercepted earlier
    /// in `handle_key_down`, before this method ever runs, so it does not
    /// need a case here.
    ///
    /// Reads gpui's own `Keystroke` directly (`event.keystroke`, not the
    /// shell-native one `convert_keystroke` produces) because free text
    /// entry needs `key_char` (the actual typed/shifted character) and
    /// named keys (`"backspace"`, `"up"`, `"down"`, `"enter"`, `"escape"`)
    /// that the shell-native conversion's matcher-oriented shape doesn't
    /// carry.
    fn handle_palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let mods = ks.modifiers;

        match ks.key.as_str() {
            "escape" => self.palette = None,
            "enter" => {
                let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
                self.palette = None;
                if let Some(item) = selected {
                    self.dispatch_palette_item(&item, window, cx);
                }
            }
            "backspace" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.backspace();
                }
                self.sync_palette_scroll();
            }
            "up" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
                self.sync_palette_scroll();
            }
            "down" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
                self.sync_palette_scroll();
            }
            "p" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
                self.sync_palette_scroll();
            }
            "n" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
                self.sync_palette_scroll();
            }
            _ => {
                // Plain typing only: a chord that also holds ctrl/cmd/fn
                // is a shortcut, not text entry, even if the platform
                // still reports a `key_char` for it.
                if !mods.control
                    && !mods.platform
                    && !mods.function
                    && let (Some(chars), Some(palette)) =
                        (ks.key_char.as_ref(), self.palette.as_mut())
                {
                    for c in chars.chars() {
                        palette.push_char(c);
                    }
                    self.sync_palette_scroll();
                }
            }
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Task 9 instant-modal redesign (see `dialog`'s module doc): while
        // Geode's own modal (`self.modal`) OR a gpui-component `Dialog`
        // layer is open, the shell's own keymap `Matcher` must not see a
        // single keystroke — otherwise e.g. `ctrl+v` typed while choosing a
        // theme in the settings modal would *also* dispatch
        // `workspace::split_right` behind it (the modal paints above the
        // tile surface, but this on_key_down listener sits on the
        // ShellView root and still receives every raw KeyDownEvent that
        // bubbles up the dispatch tree, modal-focused or not — same
        // "delivered regardless" behavior the filter-input guard below
        // already relies on). `window.has_active_dialog` is kept alongside
        // `self.modal.is_some()`, not replaced by it: gpui-component's own
        // popovers (e.g. a `Select` dropdown's overlay, reachable from
        // inside the settings modal's content) still open through that
        // crate's dialog-layer machinery, so this guard still has to
        // account for it even though nothing in this crate opens a
        // gpui-component `Dialog` directly anymore.
        //
        // Escape is the one key this branch still acts on itself — closing
        // our own modal, same as a backdrop click (`dialog::render_modal`).
        // There is no separate "Dialog" action-context to defer to anymore
        // (that was gpui-component's own `Cancel`/`Confirm` action binding,
        // scoped to its dialog's focused root): our modal is plain chrome,
        // not an action-dispatch layer, so this is the only place Escape
        // gets handled for it. Note the same "an Escape inside a focused
        // gpui-component `Input` propagates and reaches our root handler"
        // behavior the filter-input guard below documents applies here too
        // — e.g. Esc typed into the settings modal's search field still
        // reaches this branch and closes the modal, matching the old
        // dialog's UX close enough (Task 9 design note).
        if self.modal.is_some() || window.has_active_dialog(cx) {
            if self.modal.is_some() {
                // Part B: offer the modal's own key handler (if any) first
                // refusal — the keybinding dialog's vim nav/rebind-capture
                // seam (`dialog::ModalKeyHandler`, see its own doc comment
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
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "escape" {
                    self.modal = None;
                    cx.notify();
                }
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
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
            return;
        }

        // Converted once and reused below — the palette-toggle check and
        // the closed-palette dispatch both need it, and re-converting the
        // same raw event twice was pure waste.
        let keystroke = convert_keystroke(&event.keystroke);

        if let Some(ks) = &keystroke
            && self.is_palette_toggle(ks)
        {
            self.toggle_palette();
            cx.notify();
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(event, window, cx);
            cx.notify();
            return;
        }

        let Some(keystroke) = keystroke else {
            return;
        };
        let stack = self.context_stack();
        match self.matcher.press(&self.services.keymap, keystroke, &stack) {
            MatchResult::Matched(action) => {
                self.dispatch(&action, window, cx);
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

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply the UI font size (see the `fontsize` module doc): the rem
        // size scales every rem-based text size in the shell. Guarded so
        // the setter only runs on an actual change, not every frame.
        let rem = gpui::px(self.font_size.rem_px());
        if window.rem_size() != rem {
            window.set_rem_size(rem);
        }

        // `viewport_size` is the drawable area (excludes window chrome),
        // which is what `Tree::layout` should partition (gpui/window.rs).
        // Task 4 adds a top toolbar (the native title bar,
        // `TITLE_BAR_HEIGHT`) and a left sidebar (`sidebar::WIDTH`) on top
        // of the existing bottom status bar (`status::HEIGHT`); the tile
        // area gets the viewport minus all three, and `Tree::layout` is
        // still called exactly once, over those shrunk bounds.
        let viewport = window.viewport_size();
        let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
        let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
        let content_height =
            (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);

        let (focused, rects) = {
            let tree = self.services.workspaces.active();
            (
                tree.focused(),
                tree.layout(Rect {
                    x: 0.0,
                    y: 0.0,
                    w: tile_width,
                    h: content_height,
                }),
            )
        };

        // Fixed-size (not `size_full`) so it never competes with the
        // sidebar/status bar for space: the tile tree is laid out over
        // exactly this rect above, and the container must match.
        let mut surface = div()
            .relative()
            .w(px(tile_width))
            .h(px(content_height))
            .flex_none();
        if rects.is_empty() {
            surface = surface.flex().items_center().justify_center().child(
                div()
                    // Test-only hook (no-op outside test/test-support
                    // builds): lets a `#[gpui::test]` confirm this branch
                    // actually painted via `VisualTestContext::debug_bounds`
                    // — gpui's test API has no way to inspect painted text
                    // content itself, so this is the closest honest check
                    // available for "the hint painted".
                    .debug_selector(|| "empty-hint".to_string())
                    .text_color(cx.theme().muted_foreground)
                    .child("ctrl+h / ctrl+v to open a tile"),
            );
        } else {
            for (id, r) in rects {
                let is_focused = focused == Some(id);
                surface = surface.child(
                    div()
                        .absolute()
                        .left(px(r.x + 1.0))
                        .top(px(r.y + 1.0))
                        .w(px((r.w - 2.0).max(0.0)))
                        .h(px((r.h - 2.0).max(0.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(cx.theme().background)
                        .border_color(if is_focused {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                        .when(is_focused, |el| el.border_2())
                        .when(!is_focused, |el| el.border_1())
                        // `fonts::MONO` (Task 10): this placeholder label
                        // stands in for real tile content until modules
                        // land — the phase-3 blotter is what will actually
                        // fill these tiles, and it'll use `fonts::MONO` for
                        // its cells too, so the placeholder previews that
                        // face rather than the default UI one.
                        .font_family(fonts::MONO)
                        .text_color(cx.theme().muted_foreground)
                        // Click-to-focus is a convenience: keyboard (hjkl)
                        // remains the primary path through the same
                        // `Tree::focus` verb `apply_workspace_action` uses.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, _event, _window, cx| {
                                view.services.workspaces.active_mut().focus(id);
                                cx.notify();
                            }),
                        )
                        .child(format!("tile {}", id.0)),
                );
            }
        }

        let active_index = self.services.workspaces.active_index();
        let non_empty = self.services.workspaces.non_empty_indices();
        let reload_message = self.last_reload.status_message();
        let status_bar = status::status_bar(
            self.matcher.pending(),
            reload_message.as_deref(),
            self.services.theme.active_name(),
            cx,
        );
        let sidebar = sidebar::sidebar(active_index, &non_empty, cx);
        let toolbar = toolbar::toolbar(&self.filter_input, cx);

        let body = h_flex()
            .w_full()
            .h(px(content_height))
            .flex_none()
            .child(sidebar)
            .child(surface);

        let width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);

        // Which-key hint (Task 8): only computed while a sequence is
        // actually pending — `continuations` over an empty `pending` would
        // be well-defined (every binding "strictly extends" it) but the
        // overlay has nothing to say when no sequence is in flight, so it
        // must not appear then. Display-only: this reads `self.matcher`
        // without touching it, so it can never affect what `handle_key_down`
        // does with the next keystroke.
        let pending = self.matcher.pending();
        let which_key_continuations = (!pending.is_empty()).then(|| {
            whichkey::continuations(&self.services.keymap, pending, &self.context_stack())
        });
        let registry = &self.services.registry;

        // Task 9 instant-modal redesign: clone the two cheap pieces
        // (`title`: `SharedString`, `build`: `Rc<dyn Fn>`) out of `self.
        // modal` up front — the same "extract into a local ahead of the
        // render chain" move `pending`/`registry` just above already make,
        // one field further in. See `ShellModal`'s own doc comment for why
        // this step is required rather than just reading `self.modal.
        // as_ref()` inline inside the `when_some` below.
        let modal = self
            .modal
            .as_ref()
            .map(|modal| (modal.title.clone(), modal.build.clone()));

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(toolbar)
            .child(body)
            .child(status_bar)
            // The palette overlay paints above the tiles/status bar (later
            // children paint above earlier siblings) but below gpui-
            // component's own dialog/notification layers below.
            .when_some(self.palette.as_ref(), |el, state| {
                el.child(palette::render(
                    state,
                    &self.palette_scroll,
                    width,
                    viewport_height,
                    cx,
                ))
            })
            // The modal overlay paints above the palette (later children
            // paint above earlier siblings) but still below gpui-
            // component's own dialog/notification layers below — Task 9
            // instant-modal redesign, see `dialog`'s module doc. `palette`
            // is always `None` by the time `modal` is `Some`
            // (`open_shell_dialog` closes it on open), so this and the
            // block above never both add a child in the same frame, but
            // the ordering here is what would govern it if that ever
            // changed.
            .when_some(modal, |el, (title, build)| {
                let content = build(self, window, cx);
                el.child(dialog::render_modal(
                    title,
                    content,
                    width,
                    viewport_height,
                    cx,
                ))
            })
            // Painted after (so above) the modal for the same reason as the
            // modal-vs-palette ordering above: never both `Some` in the same
            // frame, but the ordering here is what would govern it if that
            // ever changed. This one, though, is a real invariant rather
            // than an incidental one — while the modal is open, `self.
            // matcher` can never go pending at all: `open_shell_dialog`
            // cancels it on open, and `handle_key_down`'s modal branch
            // returns before ever reaching `self.matcher.press` for as long
            // as `self.modal` stays `Some`, so `which_key_continuations`
            // (computed from `self.matcher.pending()`, just above) is always
            // `None` whenever `modal` is `Some`.
            .when_some(which_key_continuations, |el, continuations| {
                el.child(whichkey::render(
                    &continuations,
                    registry,
                    width,
                    status::HEIGHT,
                    cx,
                ))
            })
            // ShellView is the first-level view Root wraps; Root's own
            // Render impl does not paint these overlay layers itself, so
            // whoever it wraps must (spec: gpui-component usage.md "Overlay
            // Layers"). Task 6's palette/dialogs need this in place now.
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::{BUILTIN_KEYMAP, default_mod, register_builtin_actions};
    use crate::keymap::build_keymap;
    use geode_core::config::{ConfigSources, Layer, LayerDoc};
    // `WindowExt` is already brought in by `use super::*` (top-of-file
    // import, needed by `handle_key_down`'s dialog guard below).

    fn test_services() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
        }
    }

    /// The empty-workspace hint (`"ctrl+h / ctrl+v to open a tile"`) paints
    /// when there are no tiles. gpui's test API (`painted_quads`) has no way
    /// to inspect painted *text* content directly, so this asserts what it
    /// can see honestly: the hint's container div — tagged with a
    /// test-only `debug_selector` (a no-op outside test builds, see the
    /// comment at its call site in `Render for ShellView`) — actually
    /// painted, with real (non-zero) bounds, and that painting it produced
    /// at least one quad in the scene. This does not prove the glyphs
    /// themselves rasterized correctly — a known limitation of gpui's
    /// current test surface, not something this test can close.
    #[gpui::test]
    fn empty_workspace_paints_the_hint(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let tile_count = window.root(&mut cx).unwrap().read_with(&cx, |root, cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
                .read(cx)
                .services
                .workspaces
                .active()
                .tiles()
                .len()
        });
        assert_eq!(tile_count, 0, "sanity: workspace starts with no tiles");

        let hint_bounds = cx.debug_bounds("empty-hint");
        assert!(
            hint_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the empty-hint div should have painted with non-zero bounds, got {hint_bounds:?}"
        );

        let quads = cx.update(|window, _cx| window.painted_quads().len());
        assert!(
            quads > 0,
            "painting the empty-hint branch should have produced at least one quad"
        );
    }

    /// End-to-end: a real `ctrl+v` keystroke, dispatched through gpui's own
    /// key-event pipeline (not called directly), lands on `ShellView` and
    /// changes workspace state. Exercises `convert_keystroke` -> `Matcher`
    /// -> `apply_workspace_action` wired the way the render path wires them.
    #[gpui::test]
    fn ctrl_v_keystroke_splits_the_active_workspace(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        // Force a paint so the key-listener dispatch tree is registered
        // before we simulate a keystroke against it.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("ctrl-v");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "ctrl+v (workspace::split_right) should have created the first tile \
             on the empty starting workspace"
        );

        // The tile render path (Task 3) paints a background/border quad per
        // visible tile, not just text; a non-empty scene after the split is
        // cheap evidence the tiling surface actually drew something (the
        // geometry itself is tiling::tree's job, already unit-tested there).
        let quads_after_split = cx.update(|window, _cx| window.painted_quads().len());
        assert!(
            quads_after_split > 0,
            "expected the single tile to paint at least one quad"
        );
    }

    /// End-to-end: the direct focus bindings `mod+h`/`mod+l`
    /// (Alt, the default `mod` alias) move focus between
    /// two tiles created via the new split bindings — `ctrl+h`
    /// (`workspace::split_down`, which on the empty starting workspace just
    /// opens the first tile per `Tree::split`'s documented "split verbs
    /// double as open a tile" behavior) then `ctrl+v`
    /// (`workspace::split_right`, side by side), leaving focus on the new
    /// (right) tile. `mod+h` must move focus to the left tile, and
    /// `mod+l` back to the right one.
    #[gpui::test]
    fn ctrl_h_then_mod_hl_moves_focus_between_tiles(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-h");
        cx.simulate_keystrokes("ctrl-v");

        let right_tile =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());

        cx.simulate_keystrokes("alt-h");
        let after_left =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_ne!(
            after_left, right_tile,
            "mod+h (workspace::focus_left) should have moved focus off the right \
             tile"
        );

        cx.simulate_keystrokes("alt-l");
        let after_right =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_eq!(
            after_right, right_tile,
            "mod+l (workspace::focus_right) should have moved focus back to the \
             right tile"
        );
    }

    /// End-to-end: `shift+left` (`workspace::resize_left`, a direct binding —
    /// no mode) moves the divider adjacent to the focused tile leftward by
    /// `tiling::RESIZE_STEP` — real key dispatch all the way to
    /// `Tree::move_divider`. Focus here is the rightmost tile (no divider
    /// on its right), so the only divider available is its left one; moving
    /// it left widens the focused tile (the edge-flip case documented on
    /// `Tree::move_divider`).
    #[gpui::test]
    fn shift_left_keystroke_moves_the_left_divider(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Two tiles side by side (0.5/0.5 by default); focus lands on the
        // second (right) tile after the second split.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");

        cx.simulate_keystrokes("shift-left");

        let focused_width = shell.read_with(&cx, |shell, _| {
            let tree = shell.services.workspaces.active();
            let rects = tree.layout(Rect::UNIT);
            rects
                .into_iter()
                .find(|(id, _)| Some(*id) == tree.focused())
                .unwrap()
                .1
                .w
        });
        assert!(
            (focused_width - (0.5 + crate::tiling::RESIZE_STEP)).abs() < 1e-4,
            "shift+left should have widened the focused (rightmost) tile by moving \
             its left divider left by RESIZE_STEP, got width {focused_width}"
        );
    }

    /// End-to-end (ledgered from 1b-ui T3): a real mouse-down at a
    /// non-focused tile's on-screen coordinates focuses it, exercising the
    /// `on_mouse_down` handler wired up in `Render for ShellView` (not the
    /// keyboard path). Two tiles side by side; `mod+h` first moves focus
    /// off the freshly-split (right) tile so the click has something to
    /// change. The click point is derived from the same layout `render`
    /// itself uses — `Tree::layout` over the tile area, offset by the
    /// sidebar/toolbar chrome (`sidebar::WIDTH`, `TITLE_BAR_HEIGHT`; see
    /// CLAUDE.md's chrome-offset note) — rather than a hand-guessed pixel,
    /// so the test tracks the real geometry instead of duplicating it.
    #[gpui::test]
    fn mouse_down_on_a_tile_focuses_it(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Two tiles side by side; move focus to the left tile so the right
        // tile (about to be clicked) starts out unfocused.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-h");

        let before_focus =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());

        // Same layout math as `Render for ShellView`: the tile area is the
        // viewport minus the toolbar, sidebar, and status bar.
        let (target_id, click_point) = cx.update(|window, cx| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);

            let rects = shell.read(cx).services.workspaces.active().layout(Rect {
                x: 0.0,
                y: 0.0,
                w: tile_width,
                h: content_height,
            });
            let (id, r) = rects
                .into_iter()
                .find(|(id, _)| Some(*id) != before_focus)
                .expect("a second, non-focused tile exists");
            let point = gpui::point(
                px(sidebar::WIDTH + r.x + r.w / 2.0),
                px(toolbar_height + r.y + r.h / 2.0),
            );
            (id, point)
        });

        cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());

        let after_focus =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_eq!(
            after_focus,
            Some(target_id),
            "a mouse-down inside a non-focused tile should have focused it \
             (before: {before_focus:?}, clicked tile: {target_id:?}, after: {after_focus:?})"
        );
        assert_ne!(
            after_focus, before_focus,
            "the click should have changed which tile is focused"
        );
    }

    /// End-to-end: `ctrl+shift+right` (`workspace::move_right`, a direct
    /// binding) swaps the focused tile with
    /// its right neighbor, focus following the moved tile.
    #[gpui::test]
    fn ctrl_shift_right_keystroke_swaps_the_focused_tile_with_its_right_neighbor(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Two tiles side by side; focus is on the second (right) tile.
        // Move focus to the left tile first, then swap it rightward.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-h");

        let focused = shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        let before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().layout(Rect::UNIT)
        });

        cx.simulate_keystrokes("ctrl-shift-right");

        let after_focused =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        let after = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().layout(Rect::UNIT)
        });
        assert_eq!(
            after_focused, focused,
            "move_right keeps focus on the same TileId"
        );
        assert_ne!(
            before, after,
            "ctrl+shift+right should have swapped the two tiles' positions"
        );
    }

    /// End-to-end: `ctrl+shift+w` (`workspace::close_tile`, rebound from
    /// `mod+shift+q`) closes the focused tile.
    #[gpui::test]
    fn ctrl_shift_w_keystroke_closes_the_focused_tile(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tiles()
                .len()),
            2
        );

        cx.simulate_keystrokes("ctrl-shift-w");

        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "ctrl+shift+w (workspace::close_tile) should have closed the focused tile"
        );
    }

    /// End-to-end: a real `mod+shift+t` keystroke, dispatched through gpui's
    /// own key-event pipeline, flips the active theme's mode. Exercises the
    /// same wiring as `ctrl_v_keystroke_splits_the_active_workspace` above,
    /// but through the `theme::toggle_mode` branch of `handle_key_down`
    /// added in Task 5.
    #[gpui::test]
    fn mod_shift_t_keystroke_toggles_the_theme_mode(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });

        cx.simulate_keystrokes("alt-shift-t");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let after = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_ne!(
            before, after,
            "alt-shift-t (mod+shift+t = theme::toggle_mode) should have changed the active theme"
        );
    }

    /// Layers a test-only `"g g"` sequence binding on top of the builtin
    /// keymap (spec §3.4: sequence bindings), so the status bar's
    /// pending-keystroke display (Task 4) has something real to show. The
    /// builtin keymap has no sequence bindings anymore (move-tile went
    /// direct to `ctrl+shift+arrows`), so this isolated binding is the way
    /// tests exercise a pending keystroke at all.
    fn test_services_with_gg_binding() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        registry
            .register(crate::actions::ActionDef {
                id: crate::actions::ActionId("test::gg".to_string()),
                title: "Test gg".to_string(),
                category: "Test".to_string(),
            })
            .unwrap();
        let mod_alias = default_mod();
        let builtin_doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let user_doc = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".to_string(),
            file: "<test:user>".into(),
            table: "[[bindings]]\n[bindings.keys]\n\"g g\" = \"test::gg\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[builtin_doc, user_doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
        }
    }

    /// Pressing the first `g` of a `"g g"` sequence leaves the matcher
    /// pending (which the status bar renders as `"g"`) and the window still
    /// draws cleanly — the status bar's pending-keystroke path is live end
    /// to end through the real key-event pipeline. Task 8: the which-key
    /// overlay (`whichkey-overlay`, same `debug_selector` test hook as the
    /// empty-workspace hint) must be absent before any key is pressed and
    /// painted with real bounds once the `g` is pending.
    #[gpui::test]
    fn first_key_of_a_sequence_leaves_pending_keys_and_still_draws(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services_with_gg_binding(), None, None, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            cx.debug_bounds("whichkey-overlay").is_none(),
            "the which-key overlay must not paint while nothing is pending"
        );

        cx.simulate_keystrokes("g");

        // The pending keystroke must not stall the render thread (spec
        // PHILOSOPHY.md): the status bar draws the same frame it renders in.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let pending_len = shell.read_with(&cx, |shell, _| shell.matcher.pending().len());
        assert_eq!(
            pending_len, 1,
            "first 'g' of the 'g g' sequence should leave one pending keystroke"
        );

        let overlay_bounds = cx.debug_bounds("whichkey-overlay");
        assert!(
            overlay_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the which-key overlay should have painted with non-zero bounds while \
             pending, got {overlay_bounds:?}"
        );
    }

    /// Opening the palette while a keystroke sequence is pending cancels
    /// that pending state (Task 6: `Matcher::cancel()` on palette open —
    /// supersedes a 1b-ui deferred note that pending state would survive a
    /// palette session). Pressing the first "g" of "g g", opening then
    /// closing the palette, and pressing a fresh "g" must NOT complete the
    /// original "g g" sequence — it starts a new one instead.
    #[gpui::test]
    fn opening_the_palette_cancels_a_pending_keystroke_sequence(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services_with_gg_binding(), None, None, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("g");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
            1,
            "first 'g' of the 'g g' sequence should leave one pending keystroke"
        );

        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "ctrl-k should open the palette"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.matcher.pending().is_empty()),
            "opening the palette should cancel the pending 'g'"
        );

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "escape should close the palette"
        );

        cx.simulate_keystrokes("g");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
            1,
            "a fresh 'g' after the palette closes should start a new pending \
             sequence, not silently complete the pre-palette one"
        );
    }

    /// End-to-end command palette flow (Task 6), through the real
    /// key-event pipeline exactly like the tests above: `ctrl+k` opens it,
    /// typing "split" filters the list down to "Split right" and
    /// "Split down" — the only two titles containing that whole run
    /// as a subsequence, and, having matched the identical literal
    /// prefix "split", scored *identically* by `fuzzy_match` (verified by
    /// hand: both score 45). Which one lands at index 0 is not a
    /// fuzzy-match property; it's `PaletteState::filtered`'s stable sort
    /// preserving `build_items`' input order, which is
    /// `ActionRegistry::iter()`'s `BTreeMap<ActionId, _>` order — and
    /// `"workspace::split_down" < "workspace::split_right"` (`d` < `r`)
    /// puts "Split down" first (the rename to direction-based ids flips
    /// this tie-break from what it was under the old i3-named
    /// `split_horizontal`/`split_vertical` ids — `h` < `v` put horizontal
    /// first then; `d` < `r` puts down first now). That tie-break is
    /// deterministic (so this test is not flaky), just not the "the only
    /// match" story a prior version of this comment told. Enter then
    /// dispatches the selected item through the normal chain, closing the
    /// palette and splitting the (until then empty) active workspace.
    #[gpui::test]
    fn ctrl_k_opens_types_filters_and_enter_dispatches_the_selected_action(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "palette starts closed"
        );

        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "ctrl-k (ctrl+k = palette::toggle) should have opened the palette"
        );

        cx.simulate_input("split");
        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Split down".to_string()),
            "typing \"split\" should rank \"Split down\" first, ahead of the \
             equally-scored \"Split right\", via the registry's alphabetical \
             (d < r) ActionId order and filtered()'s stable sort"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter should close the palette"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "enter on \"Split down\" should have dispatched \
             workspace::split_down through the normal chain"
        );
    }

    /// End-to-end: Enter on a *theme* row (not an action) changes the
    /// active theme, through the same real key-event pipeline as the
    /// action-dispatch test above — the brief-mandated "theme item ->
    /// `ThemeService::apply`" path had no direct test coverage before
    /// this one; it was previously verified only by reading
    /// `dispatch_palette_item`'s source.
    ///
    /// Query "gruvbox" ranks "Theme: Gruvbox Dark" and "Theme: Gruvbox
    /// Light" identically (both match the literal, fully-consecutive run
    /// "gruvbox" right after the "Theme: " word boundary — same
    /// computation as any other title sharing that whole run, so same
    /// score); no other registered action or bundled theme title contains
    /// "gruvbox" as a subsequence at all, bundled or not, so those two are
    /// the entire tied-for-first set. As in the split-horizontal test
    /// above, which one lands at index 0 is a deterministic tie-break —
    /// `build_items` appends themes in `ThemeService::names()`'s sorted
    /// order, and `"Gruvbox Dark" < "Gruvbox Light"` alphabetically — not
    /// a property of the fuzzy match itself.
    #[gpui::test]
    fn ctrl_k_opens_types_filters_and_enter_dispatches_the_selected_theme(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let before = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_ne!(
            before, "Gruvbox Dark",
            "the starting theme must differ from the target so the assertion \
             below actually proves something changed"
        );

        cx.simulate_keystrokes("ctrl-k");
        cx.simulate_input("gruvbox");

        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Theme: Gruvbox Dark".to_string()),
            "typing \"gruvbox\" should rank \"Theme: Gruvbox Dark\" first, ahead of \
             the equally-scored \"Theme: Gruvbox Light\", via ThemeService::names()'s \
             alphabetical order and filtered()'s stable sort"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter should close the palette"
        );
        let after = shell.read_with(&cx, |shell, _| {
            shell.services.theme.active_name().to_string()
        });
        assert_eq!(
            after, "Gruvbox Dark",
            "enter on \"Theme: Gruvbox Dark\" should have dispatched it through \
             ThemeService::apply, changing the active theme"
        );
    }

    /// The full-list scroll behavior this task adds: real `down` keystrokes
    /// (not a direct `PaletteState::move_selection` call — this is the
    /// actual key-event pipeline `handle_palette_key` drives) move the
    /// selection well past `palette::VISIBLE_ROWS` (12) into rows that,
    /// before this task, `render` would never have drawn (it truncated to
    /// the top 12 filtered rows) and `move_selection`'s old clamp would
    /// never have let the selection reach. Also checks, via gpui's
    /// test-only `debug_selector`/
    /// `debug_bounds` (wired up in `palette::render`), that the selected
    /// row's *painted* bounds actually land inside the scrollable list
    /// container's bounds — proving the viewport followed the selection
    /// (`ShellView::sync_palette_scroll`'s `ScrollHandle::scroll_to_item`)
    /// rather than just moving an index nothing on screen reflects.
    #[gpui::test]
    fn arrow_down_past_visible_rows_advances_selection_and_scrolls_it_into_view(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");

        let total = shell.read_with(&cx, |shell, _| {
            shell.palette.as_ref().unwrap().filtered().len()
        });
        assert!(
            total > 20,
            "this test needs a registry+theme set with more than one \
             screenful of results (got {total}) to exercise scrolling past \
             row 12 at all"
        );

        // 20 real `down` keystrokes through the actual key-event pipeline —
        // well past the old MAX_VISIBLE=12 clamp.
        let downs = vec!["down"; 20].join(" ");
        cx.simulate_keystrokes(&downs);

        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            20,
            "20 real 'down' keystrokes should advance the selection to row \
             20, well past the old 12-row clamp"
        );

        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });

        let list_bounds = cx
            .debug_bounds("palette-list")
            .expect("the results list container should have painted");
        let row_bounds = cx
            .debug_bounds("palette-row-20")
            .expect("row 20 should still be part of the layout tree (no virtualization)");
        assert!(
            list_bounds.intersects(&row_bounds),
            "row 20 {row_bounds:?} should be scrolled into the visible list \
             viewport {list_bounds:?} after the selection moved onto it, not \
             left above/below it with only its index having changed"
        );
    }

    /// Esc closes the palette without dispatching anything — typing a
    /// query that would otherwise match and select an action must not
    /// leave any trace once the palette is dismissed.
    #[gpui::test]
    fn escape_closes_the_palette_without_dispatching(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");
        cx.simulate_input("split");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("escape");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "escape should close the palette"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "escape must not dispatch the item that was filtered/selected"
        );
    }

    /// End-to-end: palette selection wraps at both ends. Opening the palette
    /// and pressing up once (from index 0) wraps to the last filtered item.
    #[gpui::test]
    fn palette_selection_wraps_up_from_index_zero(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Open the palette with ctrl+k
        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "ctrl-k should open the palette"
        );

        // Get the filtered list length
        let filtered_len = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .map(|p| p.filtered().len())
                .unwrap_or(0)
        });
        assert!(
            filtered_len > 0,
            "palette should have at least one item when no filter is active"
        );

        // Press up once from index 0
        cx.simulate_keystrokes("up");

        // Verify we wrapped to the last item
        let selected = shell.read_with(&cx, |shell, _| {
            shell.palette.as_ref().map(|p| p.selected()).unwrap_or(0)
        });
        assert_eq!(
            selected,
            filtered_len - 1,
            "pressing up at index 0 should wrap to the last filtered item"
        );
    }

    /// Layers a user binding on top of the builtin keymap that rebinds
    /// `ctrl+k` (BUILTIN_KEYMAP's `palette::toggle` key) to
    /// `workspace::split_right` instead. Per the layering contract
    /// (last-exact-match-wins), this must fully shadow the builtin
    /// `palette::toggle` binding for that key.
    fn test_services_with_ctrl_k_rebound_to_split() -> ShellServices {
        let config = Config::load(&ConfigSources::default());
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let mod_alias = default_mod();
        let builtin_doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let user_doc = LayerDoc {
            layer: geode_core::config::Layer::User,
            name: "keymap".to_string(),
            file: "<test:user>".into(),
            table: "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"workspace::split_right\"\n"
                .parse()
                .unwrap(),
        };
        let (keymap, diags) = build_keymap(&[builtin_doc, user_doc], mod_alias, &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        ShellServices {
            config,
            registry,
            keymap,
            mod_alias,
            workspaces: Workspaces::new(),
            theme,
            session_path: None,
        }
    }

    /// Regression for `is_palette_toggle` respecting keymap layering
    /// (last-exact-match-wins, spec §3.4): a user layer rebinding `ctrl+k`
    /// away from `palette::toggle` must mean pressing it does NOT open the
    /// palette — the pre-matcher intercept in `handle_key_down` must not
    /// fire just because *some* binding for that key, anywhere in the
    /// keymap, happens to be `palette::toggle`. The rebound action
    /// (`workspace::split_right`) must dispatch instead, through the
    /// normal matcher path, proving the key was fully handed over rather
    /// than merely swallowed.
    #[gpui::test]
    fn user_layer_rebinding_ctrl_k_prevents_palette_open_and_dispatches_rebound_action(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(
                            test_services_with_ctrl_k_rebound_to_split(),
                            None,
                            None,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("ctrl-k");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "a user layer rebinding ctrl+k away from palette::toggle must shadow the \
             builtin binding — the palette must not open"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "ctrl-k should have dispatched the rebound workspace::split_right \
             action through the normal matcher path"
        );
    }

    /// Regression for `dispatch_palette_item`: selecting the
    /// `palette::toggle` row from inside the palette itself is a true
    /// toggle — the palette closes (Enter already did that) and must stay
    /// closed, not reopen. Filters straight down to that one row via its
    /// exact title so the test doesn't depend on where it ranks unfiltered.
    #[gpui::test]
    fn enter_on_the_palette_toggle_row_closes_the_palette_without_reopening(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");
        cx.simulate_input("Toggle command palette");

        let selected_title = shell.read_with(&cx, |shell, _| {
            shell
                .palette
                .as_ref()
                .and_then(PaletteState::selected_item)
                .map(|item| item.title())
        });
        assert_eq!(
            selected_title,
            Some("Toggle command palette".to_string()),
            "the query should have filtered down to exactly that row"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "enter on the palette::toggle row must leave the palette closed, not \
             reopen it"
        );
    }

    /// `apply_reload` is `ShellView`'s real config-hot-reload apply path
    /// (Task 1c-1); the watcher task is just what schedules calling it —
    /// gpui's test executor never advances its simulated clock on
    /// `run_until_parked` (confirmed against the pinned rev's
    /// `TestScheduler::run`, which is a plain `while step() {}` with no
    /// clock advancement), so there's no practical way to drive a ~500ms
    /// polling loop through a `#[gpui::test]`. These tests call
    /// `apply_reload` directly through the real entity instead — still a
    /// real-entity test, exercising the exact method the watcher calls.
    fn config_with_mod(mod_key: &str) -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", &format!("[keymap]\nmod = \"{mod_key}\"\n")).unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    /// A clean reload (no error diagnostics) is applied: the mod alias
    /// (and therefore the keymap built from it) updates to match the new
    /// config, an open palette closes (brief: "must close on a successful
    /// reload"), and the outcome is recorded as `Applied`.
    #[gpui::test]
    fn apply_reload_with_a_clean_config_applies_it_and_closes_the_palette(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Open the palette so we can prove a successful reload closes it.
        cx.simulate_keystrokes("ctrl-k");
        assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

        let new_config = config_with_mod("ctrl");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.mod_alias,
                Modifiers::CTRL,
                "a clean reload should rebuild the mod alias from the new config"
            );
            assert!(
                shell.palette.is_none(),
                "a successful reload must close an open palette"
            );
            assert_eq!(
                shell.last_reload,
                reload::ReloadOutcome::Applied { warnings: vec![] },
                "a clean reload with no diagnostics should record Applied with no warnings"
            );
        });
    }

    /// An error-severity diagnostic in the new config (here: an
    /// unsupported `config_version`) means the entire previous `Config`
    /// (and everything built from it — mod alias, keymap) is kept
    /// untouched, and the outcome records the error for the status bar.
    /// A palette open at the time stays open — only a *successful* reload
    /// closes it.
    #[gpui::test]
    fn apply_reload_with_an_error_diagnostic_keeps_last_good_config(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let original_mod_alias = shell.read_with(&cx, |shell, _| shell.services.mod_alias);

        cx.simulate_keystrokes("ctrl-k");
        assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

        // A desk-layer doc with an unsupported config_version is an error
        // diagnostic on `Config::load` itself (geode_core::config::load_layer).
        let desk = tempfile::tempdir().unwrap();
        std::fs::write(desk.path().join("app.toml"), "config_version = 99\n").unwrap();
        let bad_config = Config::load(&ConfigSources {
            builtin: vec![],
            desk: Some(desk.path().to_path_buf()),
            user: None,
        });
        assert!(
            bad_config
                .diagnostics
                .iter()
                .any(|d| d.severity == geode_core::config::Severity::Error),
            "sanity: the constructed config must actually carry an error diagnostic"
        );

        shell.update(&mut cx, |shell, cx| shell.apply_reload(bad_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.mod_alias, original_mod_alias,
                "an error diagnostic must keep the previous mod alias/keymap untouched"
            );
            assert!(
                shell.palette.is_some(),
                "a rejected reload must not close the palette"
            );
            match &shell.last_reload {
                reload::ReloadOutcome::KeptLastGood { errors } => {
                    assert_eq!(errors.len(), 1);
                    assert!(errors[0].contains("config_version"));
                }
                other => panic!("expected KeptLastGood, got {other:?}"),
            }
        });
    }

    fn config_with_theme(name: &str, mode: &str) -> Config {
        Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin(
                    "app",
                    &format!("[theme]\nname = \"{name}\"\nmode = \"{mode}\"\n"),
                )
                .unwrap(),
            ],
            desk: None,
            user: None,
        })
    }

    /// `apply_reload`'s theme-reapply guard, fired: when the new config's
    /// `[theme]` table genuinely differs from the old one's, the reload
    /// re-applies the theme, and the live `ThemeService` reflects the new
    /// value.
    #[gpui::test]
    fn apply_reload_reapplies_the_theme_when_theme_table_changed(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let mut services = test_services();
                    services.config = config_with_theme("Gruvbox", "dark");
                    let view = cx.new(|cx| {
                        // Mirrors what main.rs does before opening the
                        // window: apply the theme the starting config
                        // actually names, so this test's "old" state is a
                        // real (config, active theme) pair, not just a
                        // Default-Light service that happens to hold a
                        // Gruvbox config it never applied.
                        services.theme.apply_from_config(&services.config, cx);
                        ShellView::new(services, None, None, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .theme
                .active_name()
                .to_string()),
            "Gruvbox Dark",
            "sanity: the starting theme must actually be the one the old config names"
        );

        let new_config = config_with_theme("Default", "dark");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.theme.active_name(),
                "Default Dark",
                "a genuinely different [theme] table must be re-applied on reload"
            );
        });
    }

    /// `apply_reload`'s theme-reapply guard, holding: when the new config's
    /// `[theme]` table is identical to the old one's, the reload must NOT
    /// re-apply the theme — a runtime `theme::toggle_mode` done between the
    /// old config being applied and this reload survives untouched, rather
    /// than being silently reverted to what `[theme]` still says.
    #[gpui::test]
    fn apply_reload_preserves_a_runtime_toggle_when_theme_table_is_unchanged(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let mut services = test_services();
                    services.config = config_with_theme("Gruvbox", "dark");
                    let view = cx.new(|cx| {
                        services.theme.apply_from_config(&services.config, cx);
                        ShellView::new(services, None, None, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // A runtime toggle (mod+shift+t / theme::toggle_mode), independent
        // of config, before any reload happens.
        shell.update(&mut cx, |shell, cx| shell.services.theme.toggle_mode(cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .theme
                .active_name()
                .to_string()),
            "Gruvbox Light",
            "sanity: toggling from Gruvbox Dark should flip to Gruvbox Light"
        );

        // Same [theme] table as the config already applied — a reload
        // triggered by, say, an unrelated keymap.toml edit.
        let new_config = config_with_theme("Gruvbox", "dark");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.theme.active_name(),
                "Gruvbox Light",
                "an unchanged [theme] table must not re-apply the theme, or the \
                 runtime toggle above would be silently reverted"
            );
            assert_eq!(
                shell.last_reload,
                reload::ReloadOutcome::Applied { warnings: vec![] },
                "the reload itself still succeeds — only the theme re-apply is guarded"
            );
        });
    }

    /// Review fix round 1, Finding 2: an open palette must NOT close on a
    /// reload whose `[theme]` table is the only thing that changed — the
    /// palette's own items (Task 6: built from the registry + keymap
    /// bindings, `toggle_palette`) don't depend on `[theme]` at all, so
    /// closing it here would just be spurious churn. This is exactly the
    /// situation `theme::persist_to_user_config`'s own write triggers (see
    /// its doc comment): the app writes its own theme choice to disk, the
    /// watcher picks that up as "config changed", and this reload must not
    /// silently close a palette the user still has open.
    #[gpui::test]
    fn apply_reload_leaves_an_open_palette_open_when_only_the_theme_table_differs(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");
        assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

        // `test_services()`'s starting config has no `[theme]` table at
        // all (and no `[keymap]` table either — `config_with_theme` only
        // ever writes an "app" doc's `[theme]` section, so this new
        // config's `layered_docs("keymap")` is just as empty as the
        // starting one's, and neither sets `[keymap] mod`).
        let new_config = config_with_theme("Gruvbox", "dark");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.theme.active_name(),
                "Gruvbox Dark",
                "sanity: the theme-only change must still have been applied"
            );
            assert!(
                shell.palette.is_some(),
                "a theme-only reload must leave an open palette open"
            );
        });
    }

    /// The contrasting half of the Finding 2 pair above: a reload whose
    /// keymap docs genuinely differ (here, via `[keymap] mod`, which
    /// changes the resolved mod alias and therefore the bindings the
    /// palette would render) DOES close an open palette — its snapshot
    /// really is stale.
    #[gpui::test]
    fn apply_reload_closes_an_open_palette_when_the_keymap_docs_differ(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");
        assert!(shell.read_with(&cx, |shell, _| shell.palette.is_some()));

        let new_config = config_with_mod("ctrl");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.mod_alias,
                Modifiers::CTRL,
                "sanity: the mod alias must actually have changed"
            );
            assert!(
                shell.palette.is_none(),
                "a reload with genuinely different keymap docs must close an open palette"
            );
        });
    }

    // --- Task 3: session save/restore wiring ----------------------------

    fn test_services_with_session(session_path: std::path::PathBuf) -> ShellServices {
        let mut services = test_services();
        services.session_path = Some(session_path);
        services
    }

    /// Fix round 1, Finding 1's regression: a workspace-mutating dispatch
    /// must mark the session dirty and return *without* touching the
    /// filesystem at all — no synchronous write on the UI thread, however
    /// many dispatches fire back to back (this is exactly what OS
    /// key-repeat does to `shift+left`, ~20-30 dispatches/sec while held). The
    /// file only appears once something actually flushes
    /// `take_dirty_session_write`'s pending write — see the end-to-end test
    /// below for that half.
    #[gpui::test]
    fn dispatch_marks_the_session_dirty_without_writing_synchronously(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.toml");

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(
                            test_services_with_session(session_path.clone()),
                            None,
                            None,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // Simulate key-repeat: many workspace-mutating dispatches in a row,
        // no flush in between.
        for _ in 0..10 {
            cx.simulate_keystrokes("ctrl-v");
        }

        assert!(
            !session_path.exists(),
            "a dispatch alone must never write the session file synchronously — \
             only a flush (the watcher tick in production, taken directly in \
             tests) does"
        );

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a workspace-mutating dispatch must still mark the session dirty"
        );
    }

    /// End-to-end: real keystrokes dispatched through `ShellView` (not
    /// `apply_workspace_action` called directly) build a layout across two
    /// workspaces, marking the session dirty on each workspace-mutating
    /// dispatch (Task 3 fix round 1: no synchronous write — see the test
    /// above). The flush path (`take_dirty_session_write` +
    /// `session::write_atomic`, the same two calls the background watcher
    /// makes every ~500ms in production) is invoked directly here, since
    /// gpui's test executor never advances its simulated clock under
    /// `run_until_parked`. Loading the written file back with
    /// `session::load` — the exact function `main.rs` calls on startup —
    /// must reproduce the same layouts, and a further split on the
    /// restored `Workspaces` must allocate a `TileId` that collides with
    /// none of the restored ones (the whole reason `Workspaces::from_parts`
    /// computes `next_tile` from the restored tiles rather than resetting
    /// it to 0).
    #[gpui::test]
    fn dispatch_saves_the_session_and_it_restores_with_safe_tile_ids(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.toml");

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(
                            test_services_with_session(session_path.clone()),
                            None,
                            None,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // Build a layout on workspace 1 (two tiles side by side, then the
        // left one split stacked — three tiles total), switch to workspace
        // 2 and add a tile there too, then land back on workspace 1. Every
        // one of these is a workspace-mutating dispatch, so each marks the
        // session dirty; none of them writes anything by itself.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-h");
        cx.simulate_keystrokes("ctrl-h");
        cx.simulate_keystrokes("alt-2");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-1");

        assert!(
            !session_path.exists(),
            "no dispatch writes synchronously — the file must not exist before a flush"
        );

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Invoke the flush path directly — the same two steps the
        // background watcher's ~500ms tick performs in production.
        let pending = shell.update(&mut cx, |shell, _cx| shell.take_dirty_session_write());
        let (path, text) = pending.expect("a dirty session with a configured path must flush");
        session::write_atomic(&path, &text).unwrap();

        assert!(
            session_path.exists(),
            "the flush must have written the file"
        );
        // The atomic-write temp file (now pid+counter-suffixed, fix wave
        // Fix 2) must not be left behind.
        let leftover_tmp_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            leftover_tmp_files.is_empty(),
            "no *.tmp files should remain in the session directory, found {leftover_tmp_files:?}"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "taking the pending write must clear the dirty flag"
        );

        let live: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> =
            shell.read_with(&cx, |shell, _| {
                shell
                    .services
                    .workspaces
                    .spaces()
                    .map(|(ix, tree)| (ix, tree.layout(Rect::UNIT)))
                    .collect()
            });

        let (mut restored, warnings) = session::load(&session_path);
        assert!(warnings.is_empty(), "{warnings:?}");

        let restored_layout: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> = restored
            .spaces()
            .map(|(ix, tree)| (ix, tree.layout(Rect::UNIT)))
            .collect();
        assert_eq!(
            live, restored_layout,
            "restoring the saved session must reproduce every workspace's layout"
        );

        let before_ids: std::collections::HashSet<_> =
            restored.spaces().flat_map(|(_, t)| t.tiles()).collect();
        let new_id = restored.alloc_tile();
        assert!(
            !before_ids.contains(&new_id),
            "alloc_tile on a restored Workspaces must not collide with a restored TileId"
        );
    }

    /// `take_dirty_session_write` returns `None` (and doesn't panic) when
    /// there's nothing dirty, and again on a second call right after a
    /// flush — the dirty flag must actually be consumed, not just read.
    #[gpui::test]
    fn take_dirty_session_write_is_none_when_clean(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.toml");

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(
                            test_services_with_session(session_path.clone()),
                            None,
                            None,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert!(
            shell
                .update(&mut cx, |shell, _cx| shell.take_dirty_session_write())
                .is_none(),
            "nothing dirty yet — no pending write"
        );

        cx.simulate_keystrokes("ctrl-v");
        let first = shell.update(&mut cx, |shell, _cx| shell.take_dirty_session_write());
        assert!(
            first.is_some(),
            "the dispatch above must have marked it dirty"
        );

        assert!(
            shell
                .update(&mut cx, |shell, _cx| shell.take_dirty_session_write())
                .is_none(),
            "the dirty flag must be consumed by the first take, not left set"
        );
    }

    /// End-to-end (design doc, "Tests"): a real `mod+shift+t` keystroke,
    /// with a real `user_dir` wired up (a tempdir, exactly like
    /// `build_shell_services` wires the real `%APPDATA%`/`$HOME/.config`
    /// dir in `main.rs`), must leave the new mode written into
    /// `<user_dir>/app.toml`'s `[theme]` table on disk — the whole point of
    /// the apply-then-persist seam (`ShellView::persist_theme`) replacing
    /// the removed session `theme_mode` mechanism.
    #[gpui::test]
    fn mod_shift_t_keystroke_persists_the_new_mode_to_the_user_config_file(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let dir = tempfile::tempdir().unwrap();
        let user_dir = dir.path().to_path_buf();

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services(), None, Some(user_dir.clone()), window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            !user_dir.join("app.toml").exists(),
            "sanity: nothing written before the toggle"
        );

        cx.simulate_keystrokes("alt-shift-t");

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // `persist_theme` (Finding 1, review fix round 1) hands the actual
        // file write to `cx.background_executor()` rather than running it
        // inline, so the file doesn't necessarily exist the instant the
        // keystroke's synchronous dispatch returns — `run_until_parked`
        // drives that detached background task to completion, same pattern
        // the gpui testing reference uses for any detached background/async
        // work.
        cx.run_until_parked();

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        let active_mode = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode());
        let expected_mode = if active_mode.is_dark() {
            "dark"
        } else {
            "light"
        };

        let text = std::fs::read_to_string(user_dir.join("app.toml"))
            .expect("the keystroke must have written app.toml");
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(
            doc["theme"]["mode"].as_str(),
            Some(expected_mode),
            "the persisted [theme].mode must match the mode the keystroke applied"
        );
    }

    // --- Task 4: chrome (toolbar, sidebar, slimmed status bar) ----------

    /// Cheap evidence the new chrome actually paints something, on a
    /// window with zero tiles open — before this task, an empty workspace
    /// painted no quads at all (just the "ctrl+h / ctrl+v" placeholder
    /// text). The title bar and sidebar now fill their own background
    /// regardless of tile state, so this is a real regression check, not a
    /// tautology.
    #[gpui::test]
    fn chrome_paints_quads_even_with_no_tiles_open(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let quads = cx.update(|window, _cx| window.painted_quads().len());
        assert!(
            quads > 0,
            "the title bar and sidebar backgrounds should paint quads even \
             with no tiles open"
        );
    }

    /// Focus interplay (brief): pressing Escape while the filter input has
    /// focus hands focus back to the shell root, through the real key-event
    /// pipeline — Input's own `Escape` action handler `cx.propagate()`s (no
    /// popover/inline-completion/IME text to consume it), and
    /// `ShellView::handle_key_down`'s filter-input guard is what actually
    /// does the refocus. Focus is set directly on the input's `FocusHandle`
    /// (equivalent to what a real mouse click on it would produce) rather
    /// than simulating the click itself, since the filter field's on-screen
    /// position depends on window/text layout this test shouldn't need to
    /// know.
    #[gpui::test]
    fn escape_in_the_filter_input_returns_focus_to_the_shell_root(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let shell_focus_handle = shell.read_with(&cx, |shell, _| shell.focus_handle.clone());
        let filter_input = shell.read_with(&cx, |shell, _| shell.filter_input.clone());
        let input_focus_handle = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));

        cx.update(|window, cx| input_focus_handle.focus(window, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.update(|window, _cx| input_focus_handle.is_focused(window)),
            "sanity: focusing the input's own handle should make it focused"
        );
        assert!(
            !cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "sanity: the shell root must not be focused while the input is"
        );

        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            !cx.update(|window, _cx| input_focus_handle.is_focused(window)),
            "escape should have moved focus off the filter input"
        );
        assert!(
            cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "escape should have returned focus to the shell root"
        );
    }

    /// Focus interplay (brief): while the filter input has focus, a shell
    /// chord that has no key binding at all in the input's own gpui action
    /// context (`ctrl+h` = `workspace::split_down`) must not reach the
    /// shell's keymap `Matcher` — it stays with the input instead of
    /// splitting the workspace.
    #[gpui::test]
    fn shell_chords_do_not_fire_while_the_filter_input_has_focus(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let filter_input = shell.read_with(&cx, |shell, _| shell.filter_input.clone());
        let input_focus_handle = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));
        cx.update(|window, cx| input_focus_handle.focus(window, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("ctrl-h");

        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "ctrl+h (workspace::split_down) must not dispatch while the filter \
             input has focus"
        );
    }

    /// `settings::open` (dispatched via `ctrl+,`, the palette, or the
    /// sidebar profile icon) opens the real settings modal (Task 5, Task 9
    /// instant-modal redesign): `shell.modal` flips `Some`, and the modal
    /// chrome (`dialog::render_modal` — backdrop + panel + title row +
    /// settings content) actually paints, checked two ways: it adds quads
    /// over the empty-workspace baseline, AND its backdrop/panel
    /// `debug_selector`s recover real, non-zero-sized painted bounds — the
    /// same "prove it painted, not just that a flag flipped" standard
    /// `empty_workspace_paints_the_hint` sets. Also stands in for "the
    /// sidebar paints": its click handler calling into this exact
    /// `dispatch` path is what `sidebar::sidebar`'s profile icon wires up.
    #[gpui::test]
    fn settings_open_opens_the_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_none()),
            "sanity: no modal is open before dispatch"
        );
        let quads_before = cx.update(|window, _cx| window.painted_quads().len());

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("settings::open".to_string()), window, cx);
            });
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "settings::open should have set ShellView's own modal state"
        );

        // The workspace itself must stay untouched — settings::open is not
        // a workspace verb and must not be mistaken for one.
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(tile_count, 0);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let quads_after = cx.update(|window, _cx| window.painted_quads().len());
        assert!(
            quads_after > quads_before,
            "the modal overlay (backdrop + chrome + settings content) should \
             paint additional quads over the empty-workspace baseline"
        );

        let backdrop_bounds = cx.debug_bounds("shell-modal-backdrop");
        assert!(
            backdrop_bounds
                .is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
            "the modal backdrop should have painted with non-zero bounds, got {backdrop_bounds:?}"
        );
        let panel_bounds = cx.debug_bounds("shell-modal-panel");
        assert!(
            panel_bounds
                .is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
            "the modal panel should have painted with non-zero bounds, got {panel_bounds:?}"
        );
    }

    /// A real `ctrl+,` keystroke, through the actual key-event pipeline,
    /// dispatches `settings::open` and opens the modal — end-to-end
    /// coverage of the `BUILTIN_KEYMAP` binding added in Task 5, mirroring
    /// `mod_shift_t_keystroke_toggles_the_theme_mode` above.
    #[gpui::test]
    fn mod_comma_keystroke_opens_the_settings_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-,");

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "ctrl-, (settings::open) should have opened the settings modal"
        );
    }

    /// Fix wave, Fix 1 regression, carried forward by the Task 9
    /// instant-modal redesign: while the settings modal is open, `ctrl+v`
    /// (`workspace::split_right`) must not reach the shell's keymap
    /// `Matcher` at all — modeled on the filter-input guard this mirrors
    /// (`handle_key_down`'s early return while the filter field is
    /// focused). Before the original fix, `ShellView::handle_key_down`'s
    /// `on_key_down` listener still received every raw keystroke regardless
    /// of the dialog (dialogs paint above the tile surface but don't
    /// interrupt this view's own key dispatch); the same is true of the
    /// modal that replaced it, so a chord typed while e.g. picking a theme
    /// in the modal would silently also mutate the workspace behind it.
    /// Also checks the closed-palette case (`ctrl+k` = `palette::toggle`):
    /// that must not open either, since the palette-toggle intercept sits
    /// ahead of the matcher in `handle_key_down` and needs the same guard.
    #[gpui::test]
    fn modal_open_swallows_shell_chords(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Open the settings modal via the real `settings::open` dispatch
        // path (the builtin `ctrl+,` binding), not by constructing it
        // out-of-band, so this exercises the exact state `handle_key_down`
        // has to guard against.
        cx.simulate_keystrokes("ctrl-,");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );

        cx.simulate_keystrokes("ctrl-v");
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "ctrl+v (workspace::split_right) must not reach the matcher while \
             the settings modal is open"
        );

        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "ctrl+k (palette::toggle) must not open the command palette while \
             the settings modal is open"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "the settings modal should still be open — nothing here should \
             have closed it"
        );
    }

    /// Escape closes the modal: a real keystroke, through the actual
    /// key-event pipeline, reaching `handle_key_down`'s modal branch (not
    /// gpui-component's own `Cancel` action — there is no such layer for
    /// this modal, see `dialog`'s module doc). Mirrors `modal_open_
    /// swallows_shell_chords`'s open path, but exercises the one keystroke
    /// that must NOT be swallowed.
    #[gpui::test]
    fn escape_keystroke_closes_the_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-,");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_none()),
            "escape should have closed the modal"
        );
    }

    /// Review fix round: the module doc's "an Escape inside a focused
    /// gpui-component `Input` propagates and reaches our root handler"
    /// claim (`handle_key_down`'s modal branch doc comment) was previously
    /// only argued by analogy with the filter input — this drives the
    /// actual path it's about: the settings modal's own search field
    /// (`Settings`' sidebar header, `crates/ui/src/setting/settings.rs`
    /// pinned checkout).
    ///
    /// Focused via a real mouse click, not a direct `FocusHandle` set (the
    /// way `escape_in_the_filter_input_returns_focus_to_the_shell_root`
    /// focuses `ShellView`'s own `filter_input`) — there is no public way
    /// to reach that route here: the search field's `Entity<InputState>`
    /// lives on `SettingsState`, `pub(super)` in the pinned gpui-component
    /// checkout and reachable only from inside that crate's own `setting`
    /// module, not from this crate at all. The click point is derived from
    /// `debug_bounds("settings-content")` (a test-only hook added to
    /// `settings_view::open`'s own content wrapper) plus a small fixed
    /// offset into that wrapper's top-left corner — where the sidebar's
    /// `Input::new(&search_input)` header sits, ahead of the page list —
    /// rather than a hand-guessed absolute screen position, so the test
    /// tracks the modal's real on-screen placement instead of duplicating
    /// its layout math. The offset itself (24px right, 24px down) is an
    /// estimate from the pinned checkout's own spacing (`Sidebar::header`'s
    /// `.p_2()` plus the input control's own internal padding) landing
    /// inside the input's visible box, not a value derived from any
    /// `Input`-internal layout this crate can read — its correctness is
    /// exactly what this test's own first assertion (focus actually left
    /// the shell root) checks.
    #[gpui::test]
    fn escape_from_the_focused_settings_search_input_closes_the_modal(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        let shell_focus_handle = shell.read_with(&cx, |shell, _| shell.focus_handle.clone());

        cx.simulate_keystrokes("ctrl-,");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );
        assert!(
            cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "sanity: the shell root should still hold focus right after the \
             modal opens — nothing auto-focuses the search input"
        );

        let content_bounds = cx
            .debug_bounds("settings-content")
            .expect("the settings content wrapper should have painted bounds to click inside");
        let search_input_point = gpui::point(
            content_bounds.origin.x + px(24.0),
            content_bounds.origin.y + px(24.0),
        );

        cx.simulate_mouse_down(
            search_input_point,
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            !cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "clicking the settings search input should have moved focus off \
             the shell root and onto the input — if this fails, the click \
             point's offset likely missed the input's actual painted bounds"
        );

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_none()),
            "escape typed into the focused settings search input should \
             still have reached ShellView::handle_key_down's modal branch \
             and closed the modal — the same propagation \
             escape_in_the_filter_input_returns_focus_to_the_shell_root \
             documents for the toolbar filter field"
        );
    }

    /// Backdrop click closes the modal: a real mouse-down at a corner of
    /// the window, well outside the centered panel (`dialog::render_modal`
    /// centers the panel with `.items_center().justify_center()` over the
    /// full-viewport backdrop, so a point near the origin always falls on
    /// the backdrop, never the panel, for any viewport the test window
    /// opens at). Mirrors `mouse_down_on_a_tile_focuses_it`'s real-
    /// mouse-event structure above.
    #[gpui::test]
    fn backdrop_click_closes_the_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-,");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_mouse_down(
            gpui::point(gpui::px(4.0), gpui::px(4.0)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_none()),
            "a mouse-down on the backdrop, well outside the centered panel, \
             should have closed the modal"
        );
    }

    /// A mouse-down INSIDE the panel must not close the modal — the panel's
    /// own `on_mouse_down` (`dialog::render_modal`) stops propagation before
    /// the same bubbling event ever reaches the backdrop's close handler
    /// underneath it. The click lands on the panel's title row (top-left
    /// corner of the panel, which `dialog::render_modal` centers over the
    /// backdrop): recovered via `debug_bounds("shell-modal-panel")`, the
    /// real painted bounds, rather than recomputing the centering math by
    /// hand.
    #[gpui::test]
    fn panel_click_does_not_close_the_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-,");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );

        let panel_bounds = cx
            .debug_bounds("shell-modal-panel")
            .expect("the modal panel should have painted bounds to click inside");
        let inside_panel = gpui::point(
            panel_bounds.origin.x + gpui::px(10.0),
            panel_bounds.origin.y + gpui::px(10.0),
        );

        cx.simulate_mouse_down(inside_panel, MouseButton::Left, gpui::Modifiers::none());

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "a mouse-down inside the panel must not close the modal"
        );
    }

    /// Reproduction for the content-collapse regression: `settings_open_
    /// opens_the_modal` only proves the backdrop/panel/title chrome
    /// painted with non-zero bounds — every one of those is painted
    /// directly by `dialog::render_modal` itself, so it stays green even
    /// if the `Settings` composite nested inside the panel (theme
    /// dropdown, dark-mode switch, keyboard group) renders at zero
    /// height. This test checks the thing that test doesn't: the content
    /// wrapper `settings_view::open` tags with `debug_selector("settings-
    /// content")` must paint with a real, multi-field height (not just a
    /// non-zero sliver), and the modal must have painted meaningfully more
    /// quads than its chrome alone (backdrop + panel bg/border + title
    /// text + close button is a small, fixed handful — a fully laid out
    /// Settings composite paints far more: sidebar background, search
    /// input, page/menu row, two group boxes, a dropdown control, a
    /// switch, several labels).
    ///
    /// Uses `WindowOptions::default()`, same as every other modal test in
    /// this file — gpui's own `default_bounds` gives that a realistic
    /// 1536x1095 test window, not a cramped one, so this collapse is not
    /// an artifact of an unrealistically small test viewport.
    #[gpui::test]
    fn settings_content_paints_with_a_meaningful_height(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        let quads_before = cx.update(|window, _cx| window.painted_quads().len());

        // settings::open is bound to ctrl+, (rebound from mod+, in commit
        // 87aa731; this test merged in concurrently and carried the old key).
        cx.simulate_keystrokes("ctrl-,");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: alt-, should have opened the settings modal"
        );

        let content_bounds = cx
            .debug_bounds("settings-content")
            .expect("the settings content wrapper should have painted bounds");
        assert!(
            content_bounds.size.height >= px(200.0),
            "the settings content wrapper should paint tall enough to hold \
             the Appearance/Keyboard groups (dropdown, switch, labels) — \
             got {:?}. A collapse to a sliver height here means the Settings \
             composite's own root (which demands `size_full`, pinned \
             checkout `crates/ui/src/resizable/panel.rs`'s \
             `ResizablePanelGroup::render`) resolved its percentage height \
             against a parent with no definite height of its own.",
            content_bounds.size
        );

        let quads_after = cx.update(|window, _cx| window.painted_quads().len());
        let modal_quads = quads_after - quads_before;
        assert!(
            modal_quads >= 20,
            "opening the settings modal should paint far more than just its \
             chrome (backdrop + panel + title row + close button) — the \
             Settings composite's own sidebar/search/groups/fields should \
             contribute the bulk of it. Chrome alone paints only a handful \
             of quads; got {modal_quads} total for the whole modal, which \
             reads as chrome-only (collapsed content)."
        );
    }

    /// `settings_view::set_theme`/`set_dark_mode` are the exact handlers the
    /// dialog's theme dropdown/dark-mode switch invoke on selection/click
    /// (see those functions' doc comments: simulating a real click through
    /// the dropdown's popup-menu overlay, or the switch's own mouse
    /// handling, is impractical from a `#[gpui::test]` — this drives the
    /// identical path instead). Exercises both live-apply and the
    /// `ThemeService` bookkeeping (`active_name`/`active_mode`) staying in
    /// sync, the same contract `theme::toggle_mode` already has coverage
    /// for elsewhere in this file.
    #[gpui::test]
    fn settings_dialog_theme_and_mode_setters_apply_live_through_theme_service(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // test_services() never calls apply_from_config, so the starting
        // state is exactly load_bundled()'s own default: "Default Light",
        // mode Light (matches gpui_component::init's own initial theme).
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode()),
            crate::theme::Mode::Light,
            "sanity: the starting mode must be Light, or the assertions below \
             wouldn't prove set_dark_mode actually flipped anything"
        );

        // Fully qualified name, matching what the real dropdown passes
        // (its options come from `ThemeService::names()`, already
        // fully-qualified) — exercises `resolve`'s exact-name path
        // (`find_exact`), not the bare-family fallback (`find_family`),
        // which has its own direct coverage in `theme.rs`'s own tests.
        cx.update(|_window, cx| settings_view::set_theme(&shell, "Gruvbox Light", cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .theme
                .active_name()
                .to_string()),
            "Gruvbox Light",
            "set_theme should apply the exact fully-qualified name through \
             ThemeService::apply, regardless of the currently active mode \
             argument (find_exact ignores it)"
        );

        cx.update(|_window, cx| settings_view::set_dark_mode(&shell, true, cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .theme
                .active_name()
                .to_string()),
            "Gruvbox Dark",
            "set_dark_mode(true) should flip to the dark variant through \
             ThemeService::set_mode, staying within the same family"
        );

        cx.update(|_window, cx| settings_view::set_dark_mode(&shell, false, cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode()),
            crate::theme::Mode::Light,
            "set_dark_mode(false) should flip back to light"
        );
    }

    /// `settings_view::set_font_size` (the font-size button group's setter,
    /// driven directly for the same reason `set_theme`'s test drives the
    /// handler rather than the control) updates `ShellView::font_size`, and
    /// the next render applies it as the window's rem size — the one
    /// mechanism every path shares (see the `fontsize` module doc).
    #[gpui::test]
    fn set_font_size_applies_the_rem_size_on_the_next_render(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert_eq!(
            cx.update(|window, _cx| window.rem_size()),
            px(14.0),
            "sanity: with no [ui] font_size configured, medium (14px — one \
             step below gpui's own 16px rem default, per the fontsize \
             module doc) must be in effect after the first render"
        );

        cx.update(|_window, cx| {
            settings_view::set_font_size(&shell, crate::fontsize::FontSize::Large, cx)
        });
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.font_size),
            crate::fontsize::FontSize::Large,
            "set_font_size should update the shell's state immediately"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            cx.update(|window, _cx| window.rem_size()),
            px(16.0),
            "the render after set_font_size(Large) should apply 16px as the \
             window rem size"
        );
    }

    /// A `[ui] font_size` key already present in the layered config at
    /// startup is applied by the first render — the same read
    /// (`FontSize::from_config`) `apply_reload` re-runs on hot reload.
    #[gpui::test]
    fn a_configured_font_size_applies_from_the_first_render(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let mut services = test_services();
        services.config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfont_size = \"small\"\n").unwrap()],
            desk: None,
            user: None,
        });

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert_eq!(
            cx.update(|window, _cx| window.rem_size()),
            px(12.0),
            "[ui] font_size = \"small\" should render at a 12px rem size \
             from the very first frame"
        );
    }

    /// Task 9: opening a modal through `dialog::open_shell_dialog` (here,
    /// `settings::open` — the only current call site, migrated onto the
    /// utility) must cancel a pending keymap sequence, the same hygiene
    /// `toggle_palette` already gives palette-open. A real `g` keystroke
    /// starts the test-only `"g g"` sequence (`test_services_with_gg_binding`
    /// — the builtin keymap has no sequences of its own anymore), which
    /// leaves one pending keystroke and paints the
    /// which-key overlay (Task 8); opening the settings modal must clear
    /// both.
    #[gpui::test]
    fn modal_open_through_the_utility_clears_a_pending_sequence(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services_with_gg_binding(), None, None, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Start (but don't finish) the "g g" sequence.
        cx.simulate_keystrokes("g");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.matcher.pending().len()),
            1,
            "sanity: g alone should leave the test sequence pending"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("whichkey-overlay").is_some(),
            "sanity: the which-key overlay should paint while g is pending"
        );

        // Open the settings modal through the real dispatch path — since
        // Task 9, `settings_view::open` routes through `open_shell_dialog`.
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("settings::open".to_string()), window, cx);
            });
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "settings::open should have opened the modal"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.matcher.pending().is_empty()),
            "opening a modal through open_shell_dialog should cancel the \
             pending g sequence, same as palette-open does"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("whichkey-overlay").is_none(),
            "the which-key overlay must not paint once the pending sequence \
             has been cancelled"
        );
    }

    /// Task 9: `open_shell_dialog` must close an open palette. `settings::
    /// open` can't be reached with the palette open through its own Enter
    /// path (`dispatch_palette_item` closes the palette before dispatching
    /// anything, and `dispatch`'s `palette::toggle` arm is the only one that
    /// re-touches `self.palette` — there is no route from an open palette
    /// back into `dispatch`'s `settings::open` arm while it's still open),
    /// so this drives `dialog::open_shell_dialog` directly to exercise the
    /// utility's own hygiene in isolation from any one call site.
    #[gpui::test]
    fn open_shell_dialog_closes_an_open_palette(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "sanity: ctrl+k should open the palette"
        );

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                dialog::open_shell_dialog(
                    shell,
                    window,
                    cx,
                    "Test modal",
                    |_shell, _window, _cx| div().into_any_element(),
                );
            });
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "open_shell_dialog should have closed the open palette"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "open_shell_dialog should have set ShellView's own modal state"
        );
    }

    /// E2E test: closing a tile focuses the adjacent sibling (next in tree order),
    /// not the first leaf. Build three side-by-side tiles by splitting right twice,
    /// focus the middle one, close it, and assert focus is on the adjacent tile
    /// (which would be different from the first leaf if the old rule applied).
    #[gpui::test]
    fn close_tile_focuses_adjacent_sibling(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        // Create three tiles by splitting right twice (ctrl+v = split right).
        // First ctrl+v on empty tree creates tile 1 and focuses it.
        // Second ctrl+v creates tile 2 right of tile 1 and focuses it.
        // Third ctrl+v creates tile 3 right of tile 2 and focuses it.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");

        // Verify we have three tiles.
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(tile_count, 3, "should have created three tiles");

        // Record the tile ids in tree order before focusing the middle one.
        let tiles_before =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().tiles());
        assert_eq!(tiles_before.len(), 3);

        // After three splits, the focused tile is the last one (tiles_before[2]).
        // Focus the middle tile (at index 1) using focus_left (mod+h).
        cx.simulate_keystrokes("alt-h");

        let focused_tile =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_eq!(
            focused_tile,
            Some(tiles_before[1]),
            "should have focused the middle tile (one position left)"
        );

        // Close the middle tile (ctrl+shift+w).
        cx.simulate_keystrokes("ctrl-shift-w");

        // Verify we have two tiles left.
        let remaining_tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tiles().len()
        });
        assert_eq!(remaining_tile_count, 2, "should have two tiles after close");

        let tiles_after =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().tiles());
        // tiles_after should be [tiles_before[0], tiles_before[2]]
        assert_eq!(tiles_after, vec![tiles_before[0], tiles_before[2]]);

        // Assert that the focused tile is tiles_before[2] (the adjacent sibling in tree order).
        let focused_after_close =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());

        assert_eq!(
            focused_after_close,
            Some(tiles_before[2]),
            "closing the middle tile should focus the adjacent sibling (tiles_before[2]), \
             not the first leaf (tiles_before[0])"
        );
    }

    // --- Part B: the keybinding dialog -----------------------------------

    /// `keybindings::open` dispatch paints the modal with one row per
    /// registered action — mirrors `settings_open_opens_the_modal`'s own
    /// "prove it painted, not just that a flag flipped" standard.
    #[gpui::test]
    fn keybindings_open_paints_the_modal_with_rows(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "keybindings::open should have set ShellView's own modal state"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
            "keybindings::open should have set ShellView's own keybindings state"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let list_bounds = cx.debug_bounds("keybindings-list");
        assert!(
            list_bounds
                .is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
            "the row list should have painted with non-zero bounds, got {list_bounds:?}"
        );
        let row_0_bounds = cx.debug_bounds("keybindings-row-0");
        assert!(
            row_0_bounds
                .is_some_and(|b| b.size.width > gpui::px(0.0) && b.size.height > gpui::px(0.0)),
            "the first row should have painted with non-zero bounds, got {row_0_bounds:?}"
        );
    }

    /// Vim `/` find, end to end through real keystrokes: `/focus` jumps
    /// the selection to a matching row live, `enter` commits, `n` and
    /// `shift+n` repeat forward/backward — the wiring around the pure
    /// cores `vimfind::tests` and `keybindings_view::tests` already prove.
    #[gpui::test]
    fn slash_find_jumps_to_a_matching_row_and_n_repeats(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });

        let selected_text = |cx: &gpui::VisualTestContext| {
            shell.read_with(cx, |shell, _| {
                let rows =
                    keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
                let selected = shell.keybindings.as_ref().expect("dialog open").selected;
                keybindings_view::searchable_text(&rows[selected])
            })
        };

        cx.simulate_keystrokes("/ f o c u s");
        let live = selected_text(&cx);
        assert!(
            live.to_lowercase().contains("focus"),
            "the incremental jump should already sit on a 'focus' row before \
             enter, got {live:?}"
        );

        cx.simulate_keystrokes("enter");
        let committed = selected_text(&cx);
        assert_eq!(committed, live, "enter keeps the incremental match");

        cx.simulate_keystrokes("n");
        let next = selected_text(&cx);
        assert!(
            next.to_lowercase().contains("focus") && next != committed,
            "n should advance to a DIFFERENT matching row, got {next:?}"
        );

        cx.simulate_keystrokes("shift-n");
        assert_eq!(
            selected_text(&cx),
            committed,
            "shift+n should step back to the previous match"
        );
    }

    /// j/5j/gg/G/ctrl+d move the selection through `vimnav`, exactly as
    /// `vimnav::tests` proves the pure core does — this proves the wiring
    /// end to end, through a real keystroke and `ShellView::keybindings`.
    #[gpui::test]
    fn vim_navigation_moves_the_selection(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });

        let row_count = shell.read_with(&cx, |shell, _| {
            keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap).len()
        });
        assert!(
            row_count >= 5,
            "sanity: expected several rows, got {row_count}"
        );

        fn selected_row(shell: &Entity<ShellView>, cx: &gpui::VisualTestContext) -> usize {
            shell.read_with(cx, |shell, _| shell.keybindings.as_ref().unwrap().selected)
        }
        assert_eq!(
            selected_row(&shell, &cx),
            0,
            "sanity: starts at the top row"
        );

        cx.simulate_keystrokes("j");
        assert_eq!(selected_row(&shell, &cx), 1, "j should move down by one");

        cx.simulate_keystrokes("5 j");
        assert_eq!(
            selected_row(&shell, &cx),
            6,
            "5j should move down by five more"
        );

        cx.simulate_keystrokes("shift-g");
        assert_eq!(
            selected_row(&shell, &cx),
            row_count - 1,
            "shift+g (G) should jump to the last row"
        );

        cx.simulate_keystrokes("g g");
        assert_eq!(
            selected_row(&shell, &cx),
            0,
            "gg should jump back to the top row"
        );

        cx.simulate_keystrokes("ctrl-d");
        assert_eq!(
            selected_row(&shell, &cx),
            5,
            "ctrl+d should move down by five"
        );

        cx.simulate_keystrokes("ctrl-u");
        assert_eq!(
            selected_row(&shell, &cx),
            0,
            "ctrl+u should move back up by five, clamped at 0"
        );
    }

    /// `space` on the selected row starts listening; `escape` while
    /// listening cancels the capture WITHOUT closing the modal (falls back
    /// to ordinary dialog nav) — distinct from `escape` with nothing
    /// pending, which does close the modal (covered separately below).
    #[gpui::test]
    fn space_starts_listening_and_escape_while_listening_cancels_without_closing(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });

        cx.simulate_keystrokes("space");
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_some()),
            "space should have started listening"
        );

        cx.simulate_keystrokes("ctrl-alt-x");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .as_ref()
                .unwrap()
                .len()),
            1,
            "the keystroke should have appended to the pending capture"
        );

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_none()),
            "escape while listening should cancel the capture"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "escape while listening must NOT close the modal"
        );
    }

    /// `escape` with nothing pending (ordinary nav mode) closes the modal —
    /// same contract as every other Geode modal.
    #[gpui::test]
    fn escape_outside_listening_closes_the_modal(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });
        assert!(shell.read_with(&cx, |shell, _| shell.modal.is_some()));

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_none()),
            "escape with nothing pending should close the modal"
        );
    }

    /// End-to-end (design doc, "Tests"): listening, typing a two-keystroke
    /// sequence, then `enter` writes the new binding into the real user
    /// `keymap.toml` — verified by re-parsing the written file through the
    /// REAL production path (`LayerDoc` + `keymap::build_keymap`), the same
    /// precedent `keymap_edit`'s own round-trip tests and `shell::mod`'s
    /// `mod_shift_t_keystroke_persists_the_new_mode_to_the_user_config_file`
    /// both use for a background-executor write.
    #[gpui::test]
    fn listening_then_enter_persists_the_new_binding_to_the_user_keymap_file(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);

        let dir = tempfile::tempdir().unwrap();
        let user_dir = dir.path().to_path_buf();

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| {
                        ShellView::new(test_services(), None, Some(user_dir.clone()), window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });

        // The action bound at row 0 (top of sort order) at the moment the
        // dialog opened — what the capture below should end up bound to.
        let target_action = shell.read_with(&cx, |shell, _| {
            keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap)[0]
                .action
                .clone()
        });

        cx.simulate_keystrokes("space");
        // A two-keystroke sequence, proving multi-keystroke capture works,
        // not just a single chord.
        cx.simulate_keystrokes("ctrl-alt-x");
        cx.simulate_keystrokes("y");
        cx.simulate_keystrokes("enter");

        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_none()),
            "enter should have committed and left listening mode"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "committing a rebind must not close the dialog"
        );

        // `spawn_rebind` hands the actual write to the background executor
        // (philosophy: no I/O on the UI thread) — drive it to completion.
        cx.run_until_parked();

        let text = std::fs::read_to_string(user_dir.join("keymap.toml"))
            .expect("committing the capture must have written keymap.toml");
        let table: toml::Table = text.parse().unwrap();
        let doc = LayerDoc {
            layer: Layer::User,
            name: "keymap".to_string(),
            file: user_dir.join("keymap.toml"),
            table,
        };

        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        let (keymap, diags) = build_keymap(&[doc], default_mod(), &registry);
        assert!(
            diags.is_empty(),
            "the written keymap.toml must build clean: {diags:?}"
        );

        let binding = keymap
            .bindings()
            .iter()
            .find(|b| b.action == target_action && b.keystrokes.len() == 2)
            .expect(
                "the two-keystroke capture must have been written and resolve to the target action",
            );
        assert_eq!(binding.keystrokes[0].key, "x");
        assert!(binding.keystrokes[0].mods.ctrl && binding.keystrokes[0].mods.alt);
        assert_eq!(binding.keystrokes[1].key, "y");
        assert_eq!(binding.keystrokes[1].mods, Modifiers::NONE);
    }

    /// A real mouse click selects a different row (`debug_bounds` gives the
    /// row's real painted coordinates, same technique
    /// `mouse_down_on_a_tile_focuses_it` and the settings-panel click tests
    /// already use in this file) — then clicking that SAME, now-selected
    /// row again starts listening.
    #[gpui::test]
    fn click_selects_a_row_and_clicking_it_again_starts_listening(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();

        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("keybindings::open".to_string()), window, cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let row_1_bounds = cx
            .debug_bounds("keybindings-row-1")
            .expect("row 1 should have painted bounds to click into");
        let inside_row_1 = gpui::point(
            row_1_bounds.origin.x + gpui::px(10.0),
            row_1_bounds.origin.y + gpui::px(10.0),
        );

        cx.simulate_mouse_down(inside_row_1, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
            1,
            "clicking row 1 should have selected it"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_none()),
            "the first click on a different row must not start listening"
        );

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let row_1_bounds_again = cx
            .debug_bounds("keybindings-row-1")
            .expect("row 1 should still have painted bounds");
        let inside_row_1_again = gpui::point(
            row_1_bounds_again.origin.x + gpui::px(10.0),
            row_1_bounds_again.origin.y + gpui::px(10.0),
        );
        cx.simulate_mouse_down(
            inside_row_1_again,
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_some()),
            "clicking the already-selected row again should start listening"
        );
    }
}
