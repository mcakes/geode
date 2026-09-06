//! The shell's window root view (spec §3): a single view owning the whole
//! window contents, key dispatch, and workspace state. Chrome (Task 4):
//! `toolbar::toolbar` (the native title bar) on top, `sidebar::sidebar`
//! (workspace indicators + profile icon) on the left, `status::status_bar`
//! (pending keys, reload indicator, theme name) on the bottom. Between
//! them, the tiling tree (Task 3) renders as themed, absolutely-positioned
//! tiles over whatever rect is left. Task 6 wires the real command palette.

mod commandline_ctl;
pub mod commandline_view;
pub mod dialog;
mod drag;
mod hot_reload;
mod input;
pub mod keybindings_view;
pub mod keys;
mod occupants;
mod palette_ctl;
pub mod perf_overlay;
#[cfg(feature = "profiling")]
pub mod profiling_hook;
mod render;
mod session_io;
pub mod settings_view;
pub mod sidebar;
pub mod status;
pub mod toolbar;
pub mod whichkey;

pub use keys::convert_keystroke;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, FocusHandle, ScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::actions::ActionRegistry;
use crate::commandline::CommandLine;
use crate::fontsize::FontSize;
use crate::frame::Frame;
use crate::keymap::{Keymap, Matcher, Modifiers};
use crate::module::{ModuleRoster, TileOccupant};
use crate::palette::PaletteState;
use crate::perf::FrameHistogram;
use crate::reload;
use crate::session;
use crate::theme::ThemeService;
use crate::tiling::{TileId, Workspaces};
use crate::vimfind::FindStyle;
use geode_core::config::{Config, LayerDoc};

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
    /// The modules the app compiled in (§9.1); the shell creates tile
    /// occupants through it and never names a module crate.
    pub roster: ModuleRoster,
    /// Per-tile module kind and state restored from `session.toml`
    /// (Task 4); consumed as occupants are created.
    pub restored_tiles: crate::session::TileRecords,
}

/// What `ShellView` tells the rest of the app about a config reload (§4.5).
/// The app bridge (`geode-app`, which alone may touch `geode-data`)
/// subscribes to these to know when the views it feeds the data thread
/// need re-sending, and when to tell the user a restart is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// Views, dimensions or groupings changed and were applied; the app
    /// bridge forwards the new views to the data thread.
    ConfigReloaded,
    /// Sources or datasets changed. The data engine needs a restart to
    /// pick up new source paths or column definitions — but a `datasets`
    /// change is also a `groupings_changed` input, so the frame's own
    /// slot labels (pure presentation) are still replaced immediately;
    /// this event is only about what the data engine cannot pick up live.
    RestartRequired(String),
}

impl EventEmitter<ShellEvent> for ShellView {}

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
    /// The `/`-find behavior setting for list dialogs (vim jump vs. fzf
    /// filter — `[ui] find_style`, see `vimfind::FindStyle`). Same
    /// lifecycle as `font_size` above: resolved at startup, re-resolved on
    /// config hot reload, set directly by the settings control
    /// (`settings_view::set_find_style`) — minus the render-time apply,
    /// since there is nothing window-level to apply. Neither dialog reads
    /// it for behaviour any more: the filter-first rewrite (spec
    /// `2026-09-01-dialog-filter-input-design.md` §8) retired both `/`
    /// sessions this setting used to steer. The field, its config
    /// plumbing, and the settings row that steps it all survive whole —
    /// on purpose, for Phase 3's blotter (§9) — so this is honestly a
    /// setting with no reader today, not a live behavior switch.
    find_style: FindStyle,
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
    /// state it needs (the two list dialogs' own state lives in the
    /// sibling `keybindings`/`settings` fields below).
    modal: Option<dialog::ShellModal>,
    /// The open keybinding dialog's own pure state (Part B), or `None` when
    /// closed/never opened. Set fresh by [`keybindings_view::open`] each
    /// time (mirrors `palette`'s "nothing survives a close/reopen"
    /// contract) and read/mutated both by the modal's render closure
    /// (`keybindings_view::build`, via a plain `&ShellView` reborrow — see
    /// `ShellModal::build`'s doc comment) and by its
    /// [`dialog::ModalKeyHandler`] (`keybindings_view::handle_key`, reached
    /// through `handle_key_down`'s modal branch). Deliberately holds no
    /// `gpui` types itself (a selection index, the filter query, and the
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
    /// The open settings dialog's own pure state (the settings-dialog
    /// rewrite: the keybinding dialog's pattern applied to settings —
    /// selection and filter query, the same shape `KeybindingsState` has
    /// minus the capture sequence), or `None` when closed/never opened.
    /// Set fresh by [`settings_view::open`] each time
    /// and read/mutated by that module's `build` closure and
    /// [`dialog::ModalKeyHandler`], exactly the `keybindings` field's own
    /// contract two fields up — including holding no `gpui` types, for the
    /// same unit-testability reason. Replaces the composite-era arrangement
    /// where the settings modal kept no `ShellView` state at all (the
    /// gpui-component `Settings` composite owned its own).
    settings: Option<settings_view::SettingsState>,
    /// Scroll state for the open settings dialog's row list — the
    /// `keybindings_scroll` split, one dialog over.
    settings_scroll: ScrollHandle,
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
    /// The palette's query field (palette-input-polish task): a real
    /// gpui-component `Entity<InputState>`, replacing the hand-rolled
    /// `String` + trailing caret glyph `palette::render` used to draw
    /// itself. Built once here (like `filter_input` below — same
    /// "`Input` needs a stable entity across frames to keep its own
    /// cursor/selection/focus state" reasoning), *not* rebuilt per palette
    /// open the way `PaletteState`/`palette_scroll` are: `toggle_palette`
    /// instead resets its *value* to `""` on every open (`InputState::
    /// set_value` — deliberately chosen over a fresh entity so the same
    /// `FocusHandle` survives close/reopen, and so the one `InputEvent::
    /// Change` subscription set up in `new` below stays wired for the
    /// life of the window instead of needing to be re-subscribed on every
    /// open).
    ///
    /// **Routing** (the routing design this task settled on, verified
    /// against the pinned gpui rev's `Window::dispatch_key_event`/
    /// `dispatch_action_on_node` before writing any of this): `toggle_
    /// palette` focuses this field's `FocusHandle` on open and
    /// `close_palette` returns focus to `self.focus_handle` (the shell
    /// root) on every close path. While it's focused, gpui-component's
    /// `Input` consumes printable characters, caret movement, ctrl+a
    /// (select-all), and — new versus the old free-text palette — ctrl+v
    /// (paste) natively, via its own `KeyBinding`-bound actions
    /// (`crates/base/src/input/base/state.rs`'s `CONTEXT = "Input"`
    /// bindings): those actions run and stop propagation *before*
    /// `ShellView`'s own `on_key_down` (`handle_key_down`) ever sees the
    /// raw `KeyDownEvent` (confirmed by reading `dispatch_key_event`
    /// itself — an action handler that doesn't call `cx.propagate()`
    /// returns early without ever reaching `finish_dispatch_key_event`,
    /// which is what fires raw key listeners). Up/down, ctrl+p/ctrl+n,
    /// enter, and escape all still reach `handle_palette_key` as bubbled
    /// `KeyDownEvent`s, for three different reasons each confirmed against
    /// the pinned checkout rather than assumed: up/down have a global
    /// `KeyBinding` in the "Input" context, but the *element* only
    /// attaches an `on_action` listener for them `.when(self.is_multi_
    /// line(), ..)` — this field is single-line, so no listener exists to
    /// consume them and the raw event falls through untouched; ctrl+p and
    /// ctrl+n have no `KeyBinding` in "Input" at all (grepped the whole
    /// `crates/base/src/input` tree — absent), so they're never matched in
    /// the first place; enter and escape *are* bound and *do* have
    /// listeners (`InputBaseState::enter`/`escape`), but for a single-line,
    /// non-`clean_on_escape` input those handlers explicitly call
    /// `cx.propagate()` after emitting their `InputEvent`, letting the
    /// event continue to raw dispatch. `ctrl+k` (the palette toggle) has no
    /// "Input" binding either, so it always reaches `handle_key_down`'s
    /// earlier `is_palette_toggle` intercept regardless of focus — Escape
    /// remains the one *guaranteed* close either way. `handle_palette_key`
    /// itself now only acts on that short nav list and is a true no-op for
    /// everything else (deliberately, not via `cx.stop_propagation()` —
    /// see that method's doc comment: a typed character must keep
    /// propagating past it so the window's separate IME/text-input phase
    /// still delivers it to this field).
    palette_input: Entity<InputState>,
    /// The two list dialogs' shared filter field (the filter-first dialog
    /// UX). One entity, not one per dialog: only one modal is ever open —
    /// each dialog's own `open` returns early when `view.modal.is_some()`
    /// (`keybindings_view::open`, `settings_view::open`; the shared door
    /// `dialog::open_shell_dialog_with_key` does not guard this itself,
    /// it assigns `view.modal` unconditionally) — so they can never both
    /// want it at once. Built once here
    /// and reset by value on every open, exactly like `palette_input`
    /// above and for the same reasons — a stable `FocusHandle` across
    /// close/reopen, and one `InputEvent::Change` subscription for the
    /// life of the window instead of one per open.
    ///
    /// Deliberately blurred while the keybinding dialog is listening for
    /// a new binding: a focused `Input` consumes bare letters as text
    /// before any raw key listener sees them, so capture would be
    /// impossible otherwise (see `keybindings_view`'s module doc,
    /// "Rebind capture").
    dialog_input: Entity<InputState>,
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
    /// The tile records written by the last flush (or, before the first
    /// flush, empty), so a module state change — which never sets
    /// `session_dirty`, that flag tracks the layout only — is still
    /// noticed by the watcher's tick (Task 4).
    last_tiles_written: crate::session::TileRecords,
    /// Set by a background path that closed the palette without a `Window`
    /// to restore focus with (today: only `apply_reload`'s palette-
    /// snapshot-changed branch) — see that call site's own comment for the
    /// orphaned-`FocusId` failure mode this exists to close. Consumed at
    /// the *top* of `render`, the next place downstream that actually has
    /// a `&mut Window`: `apply_reload` already calls `cx.notify()`
    /// unconditionally, which schedules exactly the render that will pick
    /// this up, so the fix lands within one frame. `render` is the right
    /// consumption point specifically *because* `handle_key_down` is
    /// unreachable in the orphaned state this guards against (that's the
    /// whole bug) — a fix that waited for the next keystroke to run would
    /// never run at all.
    pending_focus_restore: bool,
    /// The in-flight divider drag, or `None` when no drag is active
    /// (drag-splitters task). Set by a strip's mouse-down, advanced by the
    /// full-window drag catcher's mouse-moves (live re-layout via the pure
    /// drag verbs), and cleared by its mouse-up — which is also the point
    /// the session goes dirty, so a drag-resize persists exactly like a
    /// keyboard resize (coalesced onto the same ~500ms background flush).
    /// Cancelled (top of `render`, via `cancel_divider_drag`) whenever the
    /// palette or a modal opens mid-drag, a tree tile goes fullscreen, or
    /// mod+N switches workspaces — all of which make the dragged boundary
    /// invisible or unreachable, and silently resizing an invisible layout
    /// would be a surprise on return. Cancel keeps whatever the drag
    /// already applied AND still dirties the session if it moved (review
    /// fix — "stop tracking the mouse", never "undo", and never a visible
    /// resize the next restore would lose).
    divider_drag: Option<drag::DividerDrag>,
    /// The in-flight mod+drag of a tile, or `None` (tile-drag task). Armed
    /// by a tile body's mouse-down with the configured mod key held (see
    /// [`TileDrag`] for every recorded decision), advanced by its own
    /// full-window catcher's mouse-moves, applied — through the pure
    /// `Workspace` drop verbs — only by the mouse-up's drop, and cancelled
    /// (top of `render`, `cancel_tile_drag`) by the same conditions that
    /// cancel a divider drag plus a which-key hint appearing (the hint
    /// paints over the tiles with no handlers of its own, so a drag
    /// continuing under it would target tiles the user can't fully see).
    /// Cancel applies nothing and dirties nothing — nothing has been
    /// applied yet, so unlike `divider_drag` there is no `moved`
    /// bookkeeping to preserve.
    tile_drag: Option<drag::TileDrag>,
    /// The per-tile command line's input (§3.4), built once like
    /// `palette_input` — a stable `Entity<InputState>` across frames, its
    /// value reset (not rebuilt) on every open.
    command_input: Entity<InputState>,
    /// The open command line's own pure state (§3.4), or `None` when
    /// closed. Set fresh by `open_command_line` each time (mirrors
    /// `palette`'s "nothing survives a close/reopen" contract) and read/
    /// mutated by `handle_command_line_key`/`on_command_line_changed` and
    /// painted by `commandline_view::render`.
    command_line: Option<CommandLine>,
    /// The toolbar's right-aligned filter field (Task 4). Deliberately
    /// inert — nothing reads its value; it becomes the global text filter
    /// (spec §4.1) in the data phase. Owned here (rather than built fresh
    /// per render, like `status_bar`/`sidebar`'s stateless element fns) is
    /// required: `Input` is a stateful gpui-component that needs a stable
    /// `Entity<InputState>` across frames to keep its own cursor/selection/
    /// focus state, not something rebuildable from scratch each render.
    filter_input: Entity<InputState>,
    /// Frame-time histogram (spec §7.4 — always compiled, cheap): fed at
    /// the top of `render` with the interval since the previous render.
    /// See `crate::perf`'s module doc for exactly what that signal does
    /// and doesn't capture. Owned plainly by the view — recording is a
    /// `&mut` array bump, no locks, no allocation, no extra frames.
    perf: FrameHistogram,
    /// `Instant` at the top of the previous `render` call, the other half
    /// of the frame-interval measurement. `None` until the first render
    /// (nothing to measure yet) — never reset after that: an idle gap is
    /// excluded by `perf::IDLE_CUTOFF` at record time instead.
    last_render_started: Option<std::time::Instant>,
    /// Whether the perf readout overlay is painted (`perf::toggle_overlay`,
    /// palette-reachable, bound `mod+shift+p`). Display-only: toggling it
    /// changes nothing about recording, which always runs.
    perf_overlay: bool,
    /// The shared frame (§4), created here so every occupant can hold it.
    frame: Entity<Frame>,
    /// Who lives in each tile. Created lazily in `ensure_occupants` and
    /// dropped when the tile is gone from every workspace.
    occupants: HashMap<TileId, TileOccupant>,
    /// The tiles painted last frame, to diff visibility without touching
    /// every occupant every frame.
    visible_tiles: HashSet<TileId>,
    /// Scratch storage for `ensure_occupants`'s per-frame tile-set diff
    /// (fix-round finding: `all_tiles`/`active_tiles` used to allocate a
    /// fresh `HashSet` every render). Always cleared and refilled there;
    /// empty at rest between renders, but its heap allocation survives so
    /// nothing is allocated once warm.
    scratch_all_tiles: HashSet<TileId>,
    /// Same purpose as `scratch_all_tiles`, for the active-tiles half of
    /// the diff.
    scratch_active_tiles: HashSet<TileId>,
    /// Set by `apply_reload` when a reload's `sources`/`datasets` docs
    /// (§4.5) no longer match [`sources_baseline`](Self::sources_baseline)/
    /// [`datasets_baseline`](Self::datasets_baseline) — those need a
    /// restart to take effect, unlike `groupings`/`views`/`dimensions`,
    /// which the frame picks up live. Cleared when a later reload's docs
    /// match the baseline again (M8, 3b final review: reverting the
    /// offending edit clears the message rather than leaving it up for
    /// the rest of the session). Drives the status bar's own "restart
    /// required" message, alongside the `ShellEvent::RestartRequired` the
    /// app bridge hears.
    restart_required: Option<String>,
    /// The `sources` layered doc the running `DataService` was actually
    /// built from — captured once here at construction, since a reload
    /// never rebuilds the data engine (see `apply_reload`'s doc comment).
    /// `apply_reload` compares each freshly loaded config's `sources` doc
    /// against this baseline, not against the previous reload's config,
    /// so reverting an edit back to the value the service was built from
    /// clears `restart_required`: the message means "what's on disk no
    /// longer matches what's running", and a revert makes that false
    /// again.
    sources_baseline: Vec<LayerDoc>,
    /// Same purpose as [`sources_baseline`](Self::sources_baseline), for
    /// the `datasets` doc.
    datasets_baseline: Vec<LayerDoc>,
    /// The latest data-layer diagnostic the app bridge wants shown (Phase
    /// 3 §5.1) — a source's health degrading, or events refused because
    /// the bridge's bounded channel filled up. `None` means nothing to
    /// report. The shell cannot query for itself (CLAUDE.md: it does not
    /// depend on `geode-data`), so `geode-app` is the only writer, via
    /// [`set_data_status`](Self::set_data_status); this field is plain
    /// display state, same as `restart_required` two fields up.
    data_status: Option<String>,
}

/// Whether two layered doc slices for the same config file
/// (`Config::layered_docs(name)`, Builtin → Desk → User order) are
/// identical — content, not just count. Used by [`ShellView::apply_reload`]
/// both for the keymap (Review fix round 1, Finding 2 — deciding whether a
/// reload's palette-relevant inputs actually changed) and, per-doc, for
/// deciding whether `groupings`/`views`/`dimensions`/`sources`/`datasets`
/// changed (§4.5). A free function comparing fields directly rather than a
/// `PartialEq` derive on `LayerDoc` itself (`geode_core::config`): every
/// field here already implements `PartialEq` (`Layer`, `String`, `PathBuf`,
/// `toml::Table`), so this needs no change to that shared type just for
/// these call sites.
fn docs_equal(a: &[LayerDoc], b: &[LayerDoc]) -> bool {
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
        // across frames. No placeholder — `toolbar::toolbar` names the
        // field with a search icon in the `Input`'s prefix slot instead,
        // the same way the palette and the dialogs' filter row do.
        let filter_input = cx.new(|cx| InputState::new(window, cx));

        // The palette's own query field (palette-input-polish task) — see
        // the `palette_input` field's own doc comment for the full
        // lifecycle/routing story. No placeholder text (unchanged plan
        // constraint carried over from the old hand-rolled input: an empty
        // query renders bare, no hint-text fallback).
        let palette_input = cx.new(|cx| InputState::new(window, cx));
        // One subscription for the life of the window, not re-subscribed
        // per palette open: `InputEvent::Change` only ever fires while this
        // field is actually focused (which only happens while `self.
        // palette` is open), and `toggle_palette`'s own `set_value("", ..)`
        // reset deliberately does *not* emit `Change` (`InputState::
        // set_value`'s own doc comment: it suppresses events around the
        // replace) — so this handler only ever runs for a real user edit,
        // never for the open-time reset. Feeds the new value into the
        // *pure* `PaletteState::set_query` (selection-reset-to-0 included),
        // exactly mirroring what `push_char`/`backspace` used to do
        // per-keystroke, then follows the selection change into view the
        // same way every other selection-changing path here does.
        cx.subscribe_in(&palette_input, window, |view, input, event, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let Some(palette) = view.palette.as_mut() else {
                return;
            };
            palette.set_query(input.read(cx).value().to_string());
            view.sync_palette_scroll();
            cx.notify();
        })
        .detach();

        // The per-tile command line's own input (§3.4) — same lifecycle
        // as `palette_input` above (built once, value reset on every
        // open, one `InputEvent::Change` subscription for the life of the
        // window). Unlike the palette's own subscription, this only ever
        // needs to re-rank completions — the actual key routing
        // (escape/enter/tab/ctrl+n/ctrl+p) happens in `handle_key_down`,
        // ahead of the window's own `Input` action bindings, exactly like
        // the modal branch above it.
        let command_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&command_input, window, |view, _input, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            view.on_command_line_changed(window, cx);
        })
        .detach();

        // The dialogs' shared filter field — same lifecycle as
        // `palette_input` above (see that field's doc comment), and the
        // same no-placeholder rule: `dialog::filter_row` puts a search
        // icon in the `Input`'s prefix slot instead.
        let dialog_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&dialog_input, window, |view, input, event, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let query = input.read(cx).value().to_string();
            // Route to whichever dialog is actually open. `close_modal`
            // clears both fields, so at most one is `Some` here — the
            // routing cannot land in a stale state left over from an
            // earlier open.
            if let Some(state) = view.keybindings.as_mut() {
                state.set_query(query);
                view.keybindings_scroll.scroll_to_item(0);
            } else if let Some(state) = view.settings.as_mut() {
                state.set_query(query);
                view.settings_scroll.scroll_to_item(0);
            }
            cx.notify();
        })
        .detach();

        // End any in-flight drag when the window deactivates (post-merge
        // review finding 6): cmd+tab away with the button held means the
        // release lands in some other app where no event reaches this
        // window — without this observer the stale ACTIVE drag survived,
        // and the click that re-activated the window could advance and
        // apply it. `cx.observe_window_activation` is available at the
        // pinned gpui rev (App/context.rs; the platform layer feeds it
        // from `on_active_status_change`, which macOS and Windows both
        // wire). Each drag kind ends per its own recorded semantics — the
        // same split as the Escape cancel in `handle_key_down`: a tile
        // drag CANCELS (nothing was applied, so nothing is lost) and a
        // divider drag FINISHES (its resizes were applied live and
        // persist; `cancel_divider_drag` keeps them and dirties the
        // session). The BUG 4 buttonless-move cancel remains the backstop
        // for any deactivation a platform fails to report.
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active()
                && (view.tile_drag.is_some() || view.divider_drag.is_some())
            {
                view.cancel_tile_drag();
                view.cancel_divider_drag();
                cx.notify();
            }
        })
        .detach();

        // `last_snapshot` starts empty rather than being seeded with a
        // synchronous `reload::scan` call right here: that would be real
        // filesystem I/O on the UI thread, during `new` (spec PHILOSOPHY.md
        // — review finding: the seed scan is exactly as much "the UI
        // thread" as any other poll). The watcher spawned below performs
        // the real seed scan, off-thread, as its first iteration.
        cx.spawn(async move |this, cx| {
            let mut is_first_poll = true;
            loop {
                cx.background_executor()
                    .timer(hot_reload::RELOAD_POLL_INTERVAL)
                    .await;

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
                    this.update(cx, |view, cx| view.take_dirty_session_write(cx))
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
        let find_style = FindStyle::from_config(&services.config);

        // The shared frame (§4): built from whatever `[groupings]` (plus
        // the `datasets`/`dimensions` docs it validates slots against)
        // config resolved to — see `hot_reload::rebuild_slots`, shared
        // with `apply_reload`'s own slot rebuild.
        let frame = {
            let slots = hot_reload::rebuild_slots(&services.config);
            cx.new(|_| Frame::new(slots, user_dir.clone()))
        };
        // A slot saved by a module (`:group save N`) is drained and
        // persisted here — see `on_frame_changed`'s own doc comment (§4.2:
        // the frame is pure and has no file access, so `ShellView` is the
        // one place that can do the write).
        cx.observe(&frame, |view, frame, cx| view.on_frame_changed(frame, cx))
            .detach();

        // M8: the docs the data engine actually starts with — see
        // `sources_baseline`'s field doc.
        let sources_baseline = services.config.layered_docs("sources").to_vec();
        let datasets_baseline = services.config.layered_docs("datasets").to_vec();

        Self {
            services,
            matcher: Matcher::default(),
            font_size,
            find_style,
            focus_handle,
            palette: None,
            modal: None,
            keybindings: None,
            keybindings_scroll: ScrollHandle::new(),
            settings: None,
            settings_scroll: ScrollHandle::new(),
            palette_scroll: ScrollHandle::new(),
            palette_input,
            command_input,
            command_line: None,
            dialog_input,
            desk_dir,
            user_dir,
            last_snapshot: reload::Snapshot::default(),
            last_reload: reload::ReloadOutcome::Unchanged,
            session_dirty: false,
            last_tiles_written: crate::session::TileRecords::new(),
            pending_focus_restore: false,
            divider_drag: None,
            tile_drag: None,
            filter_input,
            perf: FrameHistogram::new(),
            last_render_started: None,
            perf_overlay: false,
            frame,
            occupants: HashMap::new(),
            visible_tiles: HashSet::new(),
            scratch_all_tiles: HashSet::new(),
            scratch_active_tiles: HashSet::new(),
            restart_required: None,
            sources_baseline,
            datasets_baseline,
            data_status: None,
        }
    }

    /// Close whatever modal is open and hand focus back to the shell root
    /// — the modal-side twin of [`close_palette`](Self::close_palette),
    /// added by the filter-first dialog UX because a dialog's filter
    /// field may currently hold focus and nothing else would give it
    /// back. The one standard door for closing a modal: the escape arm in
    /// `handle_key_down`, and `dialog::render_modal`'s close-button and
    /// backdrop listeners, all go through this rather than setting
    /// `self.modal = None` directly.
    ///
    /// Also clears both dialogs' state. That is not tidiness: the shared
    /// `dialog_input` subscription routes by "whichever state is `Some`",
    /// so a stale `settings` left behind by an earlier open would
    /// swallow the *keybinding* dialog's queries.
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.settings = None;
        self.keybindings = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Fired by the `cx.observe(&frame, ..)` set up in `new` whenever the
    /// frame notifies — which covers both the keyboard's `frame::slot_*`
    /// dispatches and a module's own `:group save N`. A slot saved by a
    /// module is persisted here, off the UI thread, because the frame is
    /// pure and the module has no file access (§4.2): the frame only
    /// remembers the save in `pending_persist`, and this is where it gets
    /// drained and actually written.
    fn on_frame_changed(&mut self, frame: Entity<Frame>, cx: &mut Context<Self>) {
        if let Some((slot, grouping)) = frame.update(cx, |f, _| f.take_pending_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = crate::frame::persist_slot_to_user_config(&dir, slot, &grouping)
                    {
                        eprintln!("[groupings] warning: {e}");
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// Set a grouping slot in memory (`:group save N`, a module command —
    /// modules hold no config/file access, so this is the seam they call
    /// through). The write to the user layer's `groupings.toml` happens
    /// off the UI thread, via `on_frame_changed` observing the frame's own
    /// notify.
    pub fn save_slot(
        &mut self,
        slot: u8,
        grouping: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.frame.update(cx, |f, cx| {
            let result = f.save_slot(slot, grouping);
            if result.is_ok() {
                cx.notify();
            }
            result
        })
    }

    /// The current config, for the app bridge to read the new `views` doc
    /// out of after a `ShellEvent::ConfigReloaded` (§4.5) — `geode-app` is
    /// the only crate allowed to touch `geode-data`, so it needs to reach
    /// the reloaded config through the shell rather than reloading it a
    /// second time itself.
    pub fn config(&self) -> &Config {
        &self.services.config
    }

    /// The shared frame entity every occupant holds (§4).
    pub fn frame(&self) -> &Entity<Frame> {
        &self.frame
    }
}

#[cfg(test)]
mod tests;
