//! The shell's window root view (spec §3): a single view owning the whole
//! window contents, key dispatch, and workspace state. Chrome (Task 4):
//! `toolbar::toolbar` (the native title bar) on top, `sidebar::sidebar`
//! (workspace indicators + profile icon) on the left, `status::status_bar`
//! (pending keys, reload indicator, theme name) on the bottom. Between
//! them, the tiling tree (Task 3) renders as themed, absolutely-positioned
//! tiles over whatever rect is left. Task 6 wires the real command palette.

pub mod keys;
pub mod settings_view;
pub mod sidebar;
pub mod status;
pub mod toolbar;

pub use keys::convert_keystroke;

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, MouseButton, Window, div, px,
};
use gpui_component::input::InputState;
use gpui_component::{ActiveTheme as _, Root, TITLE_BAR_HEIGHT, h_flex, v_flex};

use crate::actions::{ActionId, ActionRegistry};
use crate::defaults::mod_alias_from_config;
use crate::keymap::{KeyContext, Keymap, MatchResult, Matcher, Modifiers, build_keymap};
use crate::palette::{self, PaletteItem, PaletteState};
use crate::reload;
use crate::session;
use crate::theme::ThemeService;
use crate::tiling::{Rect, Workspaces, apply_workspace_action};
use geode_core::config::Config;

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
    focus_handle: FocusHandle,
    /// The open command palette's state (Task 6), or `None` when closed.
    /// Built fresh from the registry/keymap/theme service each time
    /// `palette::toggle` opens it (brief: the reverse binding index is
    /// built once at palette-open, not per frame) and dropped on close —
    /// nothing about it survives being closed and reopened.
    palette: Option<PaletteState>,
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
    /// key-repeat, e.g. holding shift+h at ~20-30 events/sec). Coalescing
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

        Self {
            services,
            matcher: Matcher::default(),
            focus_handle,
            palette: None,
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
    /// close an open palette (its items snapshot the old registry/keymap at
    /// open — brief: "must close on a successful reload"), and record the
    /// outcome for the status bar.
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
        if let reload::ReloadOutcome::Applied { .. } = &outcome {
            let theme_changed =
                self.services.config.get("app", "theme") != new_config.get("app", "theme");

            self.services.config = new_config;
            self.services.mod_alias = mod_alias;
            self.services.keymap = keymap;

            if theme_changed {
                self.services
                    .theme
                    .apply_from_config(&self.services.config, cx);
            }

            self.palette = None;
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
        let bindings = palette::build_binding_index(&self.services.keymap);
        let items = palette::build_items(&self.services.registry, &self.services.theme, &bindings);
        self.palette = Some(PaletteState::new(items));
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
        } else if action.0 == "settings::open" {
            // Task 5: the real settings dialog (gpui-component's `setting`
            // module, wrapped in a `Dialog`). Reachable via `mod+,`, the
            // palette, and the sidebar profile icon.
            settings_view::open(cx.entity(), window, cx);
        }
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
        let theme_mode = if self.services.theme.active_mode().is_dark() {
            "dark"
        } else {
            "light"
        };
        let extra = session::SessionExtra {
            theme_mode: Some(theme_mode.to_string()),
        };
        match session::to_string_pretty(&self.services.workspaces, &extra) {
            Ok(text) => Some((path, text)),
            Err(e) => {
                eprintln!("[session] warning: failed to serialize session: {e}");
                None
            }
        }
    }

    /// Write the current workspace layout (and active theme mode) to the
    /// session file, if one is configured (`ShellServices::session_path`),
    /// synchronously and unconditionally (ignores `session_dirty` — this is
    /// the "flush no matter what" path, not the coalesced per-dispatch
    /// one). The only caller is `main.rs`'s best-effort `on_app_quit` hook:
    /// a one-shot at shutdown, not a per-keystroke hot path, so a
    /// synchronous atomic write (`session::save`) here is fine — it does
    /// not reintroduce the render-thread stall Task 3 fix round 1 removed
    /// from `dispatch`. A write failure (e.g. an unwritable directory) is a
    /// warning line, never a panic — session persistence is a convenience,
    /// not a correctness requirement (mirrors config's own "bad input is a
    /// warning" philosophy).
    pub fn save_session(&self) {
        let Some(path) = self.services.session_path.as_ref() else {
            return;
        };
        let theme_mode = if self.services.theme.active_mode().is_dark() {
            "dark"
        } else {
            "light"
        };
        let extra = session::SessionExtra {
            theme_mode: Some(theme_mode.to_string()),
        };
        if let Err(e) = session::save(path, &self.services.workspaces, &extra) {
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
            }
            "up" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
            }
            "down" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
            }
            "p" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
            }
            "n" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
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

        if let Some(keystroke) = convert_keystroke(&event.keystroke)
            && self.is_palette_toggle(&keystroke)
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

        let Some(keystroke) = convert_keystroke(&event.keystroke) else {
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
                el.child(palette::render(state, width, viewport_height, cx))
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
    use geode_core::config::{ConfigSources, LayerDoc};
    use gpui_component::WindowExt as _;

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

    /// End-to-end: the vim window-prefix sequence `ctrl+w h`/`ctrl+w l`
    /// (two keystrokes, `Matcher`'s sequence support) moves focus between
    /// two tiles created via the new split bindings — `ctrl+h`
    /// (`workspace::split_down`, which on the empty starting workspace just
    /// opens the first tile per `Tree::split`'s documented "split verbs
    /// double as open a tile" behavior) then `ctrl+v`
    /// (`workspace::split_right`, side by side), leaving focus on the new
    /// (right) tile. `ctrl+w h` must move focus to the left tile, and
    /// `ctrl+w l` back to the right one.
    #[gpui::test]
    fn ctrl_h_then_ctrl_w_hl_moves_focus_between_tiles(cx: &mut gpui::TestAppContext) {
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

        cx.simulate_keystrokes("ctrl-w h");
        let after_left =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_ne!(
            after_left, right_tile,
            "ctrl+w h (workspace::focus_left, a two-keystroke sequence through the \
             ctrl+w vim window prefix) should have moved focus off the right tile"
        );

        cx.simulate_keystrokes("ctrl-w l");
        let after_right =
            shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        assert_eq!(
            after_right, right_tile,
            "ctrl+w l (workspace::focus_right) should have moved focus back to the \
             right tile"
        );
    }

    /// End-to-end: `shift+h` (`workspace::resize_left`, a direct binding —
    /// no mode) grows the focused tile's edge toward the left by
    /// `tiling::RESIZE_STEP`, shrinking its left neighbor by the same
    /// amount — real key dispatch all the way to `Tree::resize`.
    #[gpui::test]
    fn shift_h_keystroke_resizes_the_focused_tile(cx: &mut gpui::TestAppContext) {
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

        cx.simulate_keystrokes("shift-h");

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
            "shift+h should have grown the focused tile leftward by RESIZE_STEP, \
             got width {focused_width}"
        );
    }

    /// End-to-end: `ctrl+w shift+l` (`workspace::move_right`, the vim
    /// window-prefix sequence's move analog) swaps the focused tile with
    /// its right neighbor, focus following the moved tile.
    #[gpui::test]
    fn ctrl_w_shift_l_keystroke_swaps_the_focused_tile_with_its_right_neighbor(
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
        cx.simulate_keystrokes("ctrl-w h");

        let focused = shell.read_with(&cx, |shell, _| shell.services.workspaces.active().focused());
        let before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().layout(Rect::UNIT)
        });

        cx.simulate_keystrokes("ctrl-w shift-l");

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
            "ctrl+w shift+l should have swapped the two tiles' positions"
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
    /// builtin keymap does start real sequences now (`ctrl+w h/j/k/l` etc.,
    /// Phase 1c), but `"g"` shares no prefix with `"ctrl+w"`, so this
    /// isolated binding stays the cheapest way to exercise a lone pending
    /// keystroke without any interaction from the real vim-prefix bindings.
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
    /// to end through the real key-event pipeline.
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
    /// key-repeat does to `shift+h`, ~20-30 dispatches/sec while held). The
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
        cx.simulate_keystrokes("ctrl-w h");
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
        assert!(
            !dir.path().join(".session.toml.tmp").exists(),
            "the atomic-write temp file must not be left behind"
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

        let (mut restored, _extra, warnings) = session::load(&session_path);
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

    /// `settings::open` (dispatched via `mod+,`, the palette, or the
    /// sidebar profile icon) opens the real settings dialog (Task 5):
    /// `window.has_active_dialog` flips true, and the dialog chrome paints
    /// additional quads over the empty-workspace baseline. Also stands in
    /// for "the sidebar paints": its click handler calling into this exact
    /// `dispatch` path is what `sidebar::sidebar`'s profile icon wires up.
    #[gpui::test]
    fn settings_open_opens_the_dialog(cx: &mut gpui::TestAppContext) {
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
            !cx.update(|window, cx| window.has_active_dialog(cx)),
            "sanity: no dialog is open before dispatch"
        );
        let quads_before = cx.update(|window, _cx| window.painted_quads().len());

        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("settings::open".to_string()), window, cx);
            });
        });

        assert!(
            cx.update(|window, cx| window.has_active_dialog(cx)),
            "settings::open should have opened a Dialog layer, tracked by \
             gpui-component's own Root state"
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
            "the dialog overlay (backdrop + chrome + settings content) should \
             paint additional quads over the empty-workspace baseline"
        );
    }

    /// A real `mod+,` keystroke, through the actual key-event pipeline,
    /// dispatches `settings::open` and opens the dialog — end-to-end
    /// coverage of the `BUILTIN_KEYMAP` binding added in Task 5, mirroring
    /// `mod_shift_t_keystroke_toggles_the_theme_mode` above.
    #[gpui::test]
    fn mod_comma_keystroke_opens_the_settings_dialog(cx: &mut gpui::TestAppContext) {
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

        // The builtin keymap's mod alias is Alt (default_mod), so `mod+,`
        // is `alt+,`.
        cx.simulate_keystrokes("alt-,");

        assert!(
            cx.update(|window, cx| window.has_active_dialog(cx)),
            "alt-, (mod+, = settings::open) should have opened the settings dialog"
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

        cx.update(|_window, cx| settings_view::set_theme(&shell, "Gruvbox", cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .theme
                .active_name()
                .to_string()),
            "Gruvbox Light",
            "set_theme should apply the named family at the currently active \
             mode (light, the starting mode here) through ThemeService::apply"
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
}
