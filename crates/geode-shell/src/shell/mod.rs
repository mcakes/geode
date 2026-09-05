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
#[cfg(test)]
use gpui::{Focusable as _, MouseButton, MouseDownEvent, MouseUpEvent, div, px};
use gpui_component::input::{InputEvent, InputState};
#[cfg(test)]
use gpui_component::{Root, TITLE_BAR_HEIGHT};

#[cfg(test)]
use crate::actions::ActionId;
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
#[cfg(test)]
use crate::tiling::{DockSide, Rect};
use crate::tiling::{TileId, Workspaces};
use crate::vimfind::FindStyle;
use geode_core::config::{Config, LayerDoc};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::schema::SchemaSpec;

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
    /// Whether the throwaway data probe is painted (`data::toggle_probe`,
    /// bound `mod+shift+d`). Deleted with the probe when the blotter
    /// lands — see [`crate::dataprobe`].
    data_probe: bool,
    /// The latest reading pushed in by the binary. The shell cannot query
    /// for itself: it does not depend on `geode-data` (CLAUDE.md), so
    /// `geode-app` owns the service and calls [`ShellView::set_probe`].
    probe: crate::dataprobe::ProbeState,
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
    /// Set by `apply_reload` when a reload changed `sources` or `datasets`
    /// (§4.5) — those need a restart to take effect, unlike `groupings`/
    /// `views`/`dimensions`, which the frame picks up live. Drives the
    /// status bar's own "restart required" message, alongside the
    /// `ShellEvent::RestartRequired` the app bridge hears.
    restart_required: Option<String>,
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
        // config resolved to. A missing doc just means an empty schema/
        // dimension set — `(SchemaSpec, Vec<Diagnostic>)` and its
        // `DerivedDimensions` twin are both `Default`, so `unwrap_or_
        // default` is a real, valid "nothing configured yet" state, not a
        // workaround.
        let frame = {
            let (schema, _) = services
                .config
                .doc("datasets")
                .map(SchemaSpec::from_doc)
                .unwrap_or_default();
            let (dims, _) = services
                .config
                .doc("dimensions")
                .map(DerivedDimensions::from_doc)
                .unwrap_or_default();
            let (slots, diags) = services
                .config
                .doc("groupings")
                .map(|d| GroupingSlots::from_doc(d, &schema, &dims))
                .unwrap_or_default();
            for d in &diags {
                eprintln!("[groupings] {d}");
            }
            cx.new(|_| Frame::new(slots, user_dir.clone()))
        };
        // A slot saved by a module (`:group save N`) is drained and
        // persisted here — see `on_frame_changed`'s own doc comment (§4.2:
        // the frame is pure and has no file access, so `ShellView` is the
        // one place that can do the write).
        cx.observe(&frame, |view, frame, cx| view.on_frame_changed(frame, cx))
            .detach();

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
            data_probe: false,
            probe: crate::dataprobe::ProbeState::default(),
            frame,
            occupants: HashMap::new(),
            visible_tiles: HashSet::new(),
            scratch_all_tiles: HashSet::new(),
            scratch_active_tiles: HashSet::new(),
            restart_required: None,
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

    /// Hand the probe a new reading (spec §7's vertical slice).
    ///
    /// The shell cannot query for itself — it does not depend on
    /// `geode-data` — so `geode-app` polls the `DataService` result
    /// channel and pushes what arrives here. Notifies unconditionally:
    /// the §7.1 budget is measured to the painted frame, so a reading that
    /// did not repaint would not have been measured.
    pub fn set_probe(&mut self, probe: crate::dataprobe::ProbeState, cx: &mut Context<Self>) {
        self.probe = probe;
        cx.notify();
    }

    /// Whether the probe is currently painted.
    pub fn data_probe_visible(&self) -> bool {
        self.data_probe
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
            roster: crate::module::ModuleRoster::default(),
            restored_tiles: crate::session::TileRecords::new(),
        }
    }

    /// `test_services` with a recording module as the default occupant.
    fn services_with_recorder() -> (
        ShellServices,
        std::rc::Rc<std::cell::RefCell<Vec<crate::module::recording::Recorded>>>,
    ) {
        let recorder = crate::module::recording::RecordingFactory::new("rec");
        let log = recorder.log.clone();
        let mut services = test_services();
        let mut roster = crate::module::ModuleRoster::new("rec");
        roster.add(Box::new(recorder));
        roster.register_actions(&mut services.registry);
        // The keymap must be rebuilt after the module's actions exist,
        // exactly as `main.rs` orders it, plus a binding into the
        // module's own context so a key can be seen to reach it.
        let doc = LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap();
        let module_doc = LayerDoc::builtin(
            "keymap",
            "[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"j\" = \"rec::noop\"\n",
        )
        .unwrap();
        let (keymap, diags) = build_keymap(&[doc, module_doc], default_mod(), &services.registry);
        assert!(diags.is_empty(), "{diags:?}");
        services.keymap = keymap;
        services.roster = roster;
        (services, log)
    }

    fn open_shell(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::shell::dialog::init_reclaimed_keybindings);
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (window, vcx)
    }

    fn shell_of(
        window: &gpui::WindowHandle<Root>,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<ShellView> {
        window.root(cx).unwrap().read_with(cx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        })
    }

    #[gpui::test]
    fn colon_opens_the_command_line_and_enter_runs_the_line_on_the_occupant(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "the strip painted"
        );
        cx.simulate_input("unpin");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_none(),
            "closed after a successful command"
        );
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert!(
            log.borrow()
                .contains(&crate::module::recording::Recorded::Command(
                    tile,
                    "unpin".into()
                )),
            "{:?}",
            log.borrow()
        );
        let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(
            cx.update(|window, _| focused.is_focused(window)),
            "focus back on the shell"
        );
    }

    #[gpui::test]
    fn completions_rank_accept_on_tab_and_submit_on_a_unique_enter(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort g");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("completion-row-0").is_some(),
            "gamma01 is offered"
        );
        assert!(
            cx.debug_bounds("completion-row-1").is_none(),
            "delta01 has no g"
        );
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
        assert_eq!(line, "sort gamma01");

        cx.simulate_keystrokes("enter");
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert!(
            log.borrow()
                .contains(&crate::module::recording::Recorded::Command(
                    tile,
                    "sort gamma01".into()
                ))
        );

        // A unique match submits without tab.
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort del");
        cx.simulate_keystrokes("enter");
        assert!(
            log.borrow()
                .contains(&crate::module::recording::Recorded::Command(
                    tile,
                    "sort delta01".into()
                )),
            "{:?}",
            log.borrow()
        );
    }

    /// C1, final review: a second `tab` used to corrupt the line, because
    /// `CommandLine::word` was only ever refreshed by
    /// `on_command_line_changed` (which `InputEvent::Change` drives), and
    /// the Accept branch's `set_value` emits no `Change` at the pinned
    /// gpui-component rev — so the *second* accept spliced the new
    /// candidate into the byte range the *first* accept had already made
    /// stale. Both `delta01` and `gamma01` match "a01" (the branch's own
    /// fixture — see `RecordingFactory::new`), so this exercises the
    /// two-candidate cycle the single-tab tests never reach a second time.
    #[gpui::test]
    fn a_second_tab_cycles_the_completion_instead_of_corrupting_the_line(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort a01");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);

        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
        assert_eq!(line, "sort delta01", "first tab accepts the top candidate");

        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let line = shell.read_with(&cx, |s, cx| s.command_input.read(cx).value().to_string());
        assert_eq!(
            line, "sort gamma01",
            "second tab cycles cleanly to the next candidate — a stale \
             `c.word` would instead splice into the wrong range and \
             produce \"sort gamma01ta01\""
        );
    }

    #[gpui::test]
    fn an_ambiguous_enter_and_a_failing_command_show_inline_and_stay_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut services, log) = services_with_recorder();
        // Make the recorder's `command` fail.
        let mut roster = crate::module::ModuleRoster::new("rec");
        let mut rec = crate::module::recording::RecordingFactory::new("rec");
        rec.command_result = Err("no such column".into());
        let log2 = rec.log.clone();
        roster.add(Box::new(rec));
        services.roster = roster;
        let _ = log;
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.simulate_input("sort a01");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let error = shell.read_with(&cx, |s, _| {
            s.command_line.as_ref().and_then(|c| c.error.clone())
        });
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains("delta01") && e.contains("gamma01")),
            "{error:?}"
        );
        assert!(
            log2.borrow()
                .iter()
                .all(|r| !matches!(r, crate::module::recording::Recorded::Command(..))),
            "nothing ran"
        );

        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("enter");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let error = shell.read_with(&cx, |s, _| {
            s.command_line.as_ref().and_then(|c| c.error.clone())
        });
        assert_eq!(
            error.as_deref(),
            Some("no such column"),
            "the occupant's error, inline, line still open"
        );
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("command-line").is_none());
    }

    #[gpui::test]
    fn slash_streams_find_events_and_escape_cancels(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("/");
        cx.simulate_input("sp");
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        use crate::module::{FindEvent, recording::Recorded};
        assert!(
            log.borrow()
                .contains(&Recorded::Find(tile, FindEvent::Changed("sp".into()))),
            "{:?}",
            log.borrow()
        );
        cx.simulate_keystrokes("escape");
        assert!(
            log.borrow()
                .contains(&Recorded::Find(tile, FindEvent::Cancelled))
        );
        cx.simulate_keystrokes("/");
        cx.simulate_input("x");
        cx.simulate_keystrokes("enter");
        assert!(
            log.borrow()
                .contains(&Recorded::Find(tile, FindEvent::Committed("x".into())))
        );
    }

    /// Fix round 1, finding 1: `ctrl+k` is a shipped, always-reachable
    /// binding, so it must still open the palette (and clean up after
    /// itself) even from inside an open `:` line, rather than the line
    /// swallowing it silently and staying stuck open.
    #[gpui::test]
    fn ctrl_k_cancels_an_open_command_line_and_opens_the_palette(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "line open before ctrl-k"
        );
        cx.simulate_keystrokes("ctrl-k");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_none(),
            "ctrl-k should have cancelled the open command line"
        );
        let shell = shell_of(&window, &mut cx);
        assert!(
            shell.read_with(&cx, |s, _| s.palette.is_some()),
            "ctrl-k should still open the palette"
        );
        // A `Command` prompt cancel is silent: nothing was submitted, and
        // (unlike `/`) nothing is cancelled on the occupant either.
        assert!(
            log.borrow().iter().all(|r| !matches!(
                r,
                crate::module::recording::Recorded::Command(..)
                    | crate::module::recording::Recorded::Find(..)
            )),
            "{:?}",
            log.borrow()
        );
    }

    /// Fix round 1, finding 1: opening a shell dialog over an open `/`
    /// line must cancel it (mirroring the existing `close_palette` call
    /// in `dialog::open_shell_dialog_with_key`) — otherwise the line
    /// stays `Some`, still painted, but the modal branch in
    /// `handle_key_down` is checked first and would swallow every key
    /// meant for it from then on. Dispatches `settings::open` directly,
    /// the same real path `settings_open_opens_the_modal` above uses,
    /// rather than a raw `ctrl-,` keystroke: this is testing what opening
    /// a dialog does to an open command line, not how the dialog itself
    /// gets reached.
    #[gpui::test]
    fn opening_a_shell_dialog_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("/");
        cx.simulate_input("sp");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "line open before the dialog"
        );
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_none(),
            "opening the dialog should have cancelled the open command line"
        );
        assert!(
            shell.read_with(&cx, |s, _| s.modal.is_some()),
            "the dialog should still have opened"
        );
        use crate::module::{FindEvent, recording::Recorded};
        assert_eq!(
            log.borrow().last(),
            Some(&Recorded::Find(tile, FindEvent::Cancelled)),
            "{:?}",
            log.borrow()
        );
    }

    /// Fix round 1, finding 2: a mouse-down on a different tile is the one
    /// way the workspace's focused tile can change while a command line
    /// is open (every keystroke is claimed ahead of the matcher), so it
    /// must cancel the line rather than leaving it painted under the
    /// wrong tile — this is what keeps "focused tile == command_line.tile
    /// while open" an invariant (see the comment where the strip is
    /// painted). Same click-point layout math as `mouse_down_on_a_tile_
    /// focuses_it` below.
    #[gpui::test]
    fn a_mouse_down_on_another_tile_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        // Two tiles, so there is a second, non-focused one to click.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        let shell = shell_of(&window, &mut cx);
        let opened_on = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "line open before the click"
        );

        let (target_id, click_point) = cx.update(|window, cx| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            let rects = shell
                .read(cx)
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect {
                    x: 0.0,
                    y: 0.0,
                    w: tile_width,
                    h: content_height,
                });
            let (id, r) = rects
                .into_iter()
                .find(|(id, _)| Some(*id) != Some(opened_on))
                .expect("a second, non-focused tile exists");
            let point = gpui::point(
                px(sidebar::WIDTH + r.x + r.w / 2.0),
                px(toolbar_height + r.y + r.h / 2.0),
            );
            (id, point)
        });

        cx.simulate_mouse_down(click_point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(click_point, MouseButton::Left, gpui::Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            cx.debug_bounds("command-line").is_none(),
            "the click should have cancelled the open command line"
        );
        let focused = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert_eq!(focused, target_id, "the click still moved focus");
        let shell_focus = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(
            cx.update(|window, _| shell_focus.is_focused(window)),
            "focus should have returned to the shell root, then re-armed \
             by the click's own restore"
        );
        assert!(
            log.borrow().iter().all(|r| !matches!(
                r,
                crate::module::recording::Recorded::Command(..)
                    | crate::module::recording::Recorded::Find(..)
            )),
            "a Command prompt's cancel is silent: {:?}",
            log.borrow()
        );
    }

    /// I1, final review: the sidebar's workspace-switch mouse-down
    /// (`sidebar::sidebar`) dispatches `workspace::switch_N` directly —
    /// there is no sidebar-click precedent to imitate instead, so this
    /// dispatches the same action the real mouse-down does, the way
    /// `switching_away_and_back_within_one_frame_voids_the_drop` already
    /// does for the identical reason. Switching workspaces changes which
    /// tile the active workspace considers focused without ever touching
    /// the command line or moving keyboard focus, so nothing in the
    /// pre-fix code cancelled the line: the strip kept painting over the
    /// OLD workspace's tile while `enter` would have run the line against
    /// a tile the switch just left. The render-time backstop (see the
    /// comment beside `ensure_occupants`'s drag-cancel neighbours in
    /// `render`) is what closes this.
    #[gpui::test]
    fn switching_workspaces_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "line open before the switch"
        );

        let shell = shell_of(&window, &mut cx);
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(
                    &ActionId("workspace::switch_2".to_string()),
                    None,
                    window,
                    cx,
                );
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // The direct state check, not just the painted strip: workspace 2
        // has no tile of its own, so `focused_rect` would be `None` there
        // regardless of whether the line was actually cancelled — the
        // strip's absence alone cannot isolate this mutation from "there
        // is nowhere to paint it this frame".
        assert!(
            shell.read_with(&cx, |s, _| s.command_line.is_none()),
            "switching workspaces should have cancelled the open command line"
        );
        assert!(
            cx.debug_bounds("command-line").is_none(),
            "and the strip should not be painted either"
        );
        assert_eq!(
            shell.read_with(&cx, |s, _| s.services.workspaces.active_index()),
            2,
            "the switch itself still happened"
        );
        assert!(
            log.borrow().iter().all(|r| !matches!(
                r,
                crate::module::recording::Recorded::Command(..)
                    | crate::module::recording::Recorded::Find(..)
            )),
            "a Command prompt's cancel is silent: {:?}",
            log.borrow()
        );
    }

    /// I1, final review, the other half: a click into the toolbar's
    /// `filter_input` steals keyboard focus from `command_input` without
    /// touching the workspace at all — the opposite failure shape from
    /// `switching_workspaces_cancels_an_open_command_line`'s above, and
    /// the other arm of the same render-time backstop's `||`. Focuses
    /// `filter_input`'s handle directly, the same real path
    /// `escape_in_the_filter_input_returns_focus_to_the_shell_root`
    /// already uses instead of a pixel-coordinate click.
    #[gpui::test]
    fn clicking_the_filter_input_cancels_an_open_command_line(cx: &mut gpui::TestAppContext) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes(":");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("command-line").is_some(),
            "line open before the filter is focused"
        );

        let shell = shell_of(&window, &mut cx);
        let filter_input = shell.read_with(&cx, |s, _| s.filter_input.clone());
        let filter_focus = filter_input.read_with(&cx, |state, cx| state.focus_handle(cx));
        cx.update(|window, cx| filter_focus.focus(window, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            cx.debug_bounds("command-line").is_none(),
            "focusing the filter input should have cancelled the open command line"
        );
        assert!(
            cx.update(|window, _| filter_focus.is_focused(window)),
            "the filter still took focus"
        );
        assert!(
            log.borrow().iter().all(|r| !matches!(
                r,
                crate::module::recording::Recorded::Command(..)
                    | crate::module::recording::Recorded::Find(..)
            )),
            "a Command prompt's cancel is silent: {:?}",
            log.borrow()
        );
    }

    #[gpui::test]
    fn a_restored_tile_of_an_unknown_kind_falls_back_without_its_state(
        cx: &mut gpui::TestAppContext,
    ) {
        // The record names a kind nothing in the roster registers, so
        // `ensure_occupants` falls back to the roster's default ("rec")
        // — but the fallback factory did not produce that state and must
        // not be handed it (fix-round finding).
        let (mut services, log) = services_with_recorder();
        let mut state = toml::Table::new();
        state.insert(
            "last_command".into(),
            toml::Value::String("state for a different module".into()),
        );
        services.restored_tiles.insert(
            1,
            crate::session::TileRecord {
                kind: "unregistered-kind".into(),
                state,
            },
        );
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert_eq!(tile, TileId(1), "the first split always allocates tile 1");
        assert_eq!(
            shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
            Some("rec"),
            "the default factory still hosts the tile"
        );
        assert!(
            log.borrow().iter().any(
                |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
            ),
            "the fallback factory got no state: {:?}",
            log.borrow()
        );
    }

    /// I2, final review: `fill_all_tiles` walks every workspace, so the
    /// FIRST render creates an occupant for a tile in an inactive
    /// workspace too — restored here via `session::from_toml`, the same
    /// real path `current_tiles_reflects_live_occupants_and_restored_
    /// state_reaches_the_factory` above builds. Before the fix, only
    /// tiles in the *active* set ever got a `set_visible` call at all;
    /// an occupant created outside it heard nothing, ever. `RecordingFactory`
    /// does not default a fresh occupant to anything — the assertion
    /// below is only meaningful because `set_visible` is required to be
    /// called at creation time, per its own doc comment's contract.
    #[gpui::test]
    fn an_occupant_created_outside_the_active_workspace_is_told_it_is_hidden(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut table = session::to_toml(&Workspaces::new(), &session::TileRecords::new());
        // Workspace 1 (the default active one) stays empty. Workspace 2
        // gets one tile, restored with the recorder's own kind — this is
        // the occupant that is created on the very first render while
        // workspace 1, not 2, is active.
        let ws2: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "rec"
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("2".to_string(), toml::Value::Table(ws2));
        }
        let restored = session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        assert_eq!(
            restored.workspaces.active_index(),
            1,
            "sanity: workspace 1, not 2, is active"
        );

        let (mut services, log) = services_with_recorder();
        services.workspaces = restored.workspaces;
        services.restored_tiles = restored.tiles;
        let (_window, mut cx) = open_shell(cx, services);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                crate::module::recording::Recorded::Visible(TileId(1), false)
            )),
            "an occupant created outside the active workspace must be told \
             it is hidden on its first render: {:?}",
            log.borrow()
        );
        assert!(
            log.borrow().iter().all(|r| !matches!(
                r,
                crate::module::recording::Recorded::Visible(TileId(1), true)
            )),
            "it must never have been told the opposite: {:?}",
            log.borrow()
        );
    }

    #[gpui::test]
    fn a_split_creates_an_occupant_of_the_default_kind_and_paints_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert_eq!(
            shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
            Some("rec")
        );
        assert!(
            log.borrow().iter().any(
                |r| matches!(r, crate::module::recording::Recorded::Created(t, None) if *t == tile)
            ),
            "{:?}",
            log.borrow()
        );
        // `debug_bounds` takes `&'static str`; leak the dynamic selector
        // (test-only, a few bytes).
        let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
        let bounds = cx.debug_bounds(selector);
        assert!(
            bounds.is_some_and(|b| b.size.width > px(0.0)),
            "the occupant's view painted: {bounds:?}"
        );
    }

    #[gpui::test]
    fn a_key_in_the_occupants_context_reaches_its_dispatch_with_the_count(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("4 j");
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        assert!(
            log.borrow().iter().any(|r| matches!(
                r,
                crate::module::recording::Recorded::Dispatch(t, a, Some(4)) if *t == tile && a.0 == "rec::noop"
            )),
            "{:?}",
            log.borrow()
        );
    }

    #[gpui::test]
    fn closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });

        cx.simulate_keystrokes("alt-2");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            log.borrow().iter().any(
                |r| matches!(r, crate::module::recording::Recorded::Visible(t, false) if *t == tile)
            ),
            "hidden on switch: {:?}",
            log.borrow()
        );
        cx.simulate_keystrokes("alt-1");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            log.borrow().iter().any(
                |r| matches!(r, crate::module::recording::Recorded::Visible(t, true) if *t == tile)
            ),
            "shown on return: {:?}",
            log.borrow()
        );

        cx.simulate_keystrokes("ctrl-w");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            shell.read_with(&cx, |s, _| s.occupant_kind(tile)),
            None,
            "occupant dropped with its tile"
        );
    }

    #[gpui::test]
    fn a_click_on_a_tile_leaves_the_shell_focused_on_the_next_frame(cx: &mut gpui::TestAppContext) {
        // gpui focuses a tracked element on mouse down; an occupant that
        // tracks its own handle (DataTable does) would take focus with it
        // and every shell chord would go dead. The tile's click handler
        // arms the same restore `apply_reload` uses (§3.3).
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        // `debug_bounds` takes `&'static str`; leak the dynamic selector
        // (test-only, a few bytes).
        let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(
            cx.update(|window, _| focused.is_focused(window)),
            "the shell root has focus again"
        );
    }

    #[gpui::test]
    fn a_click_on_a_docked_tile_leaves_the_shell_focused_on_the_next_frame(
        cx: &mut gpui::TestAppContext,
    ) {
        // Same hazard as the tree-tile test above, but for a tile parked
        // in a dock (fix-round finding: the dock-tile `on_mouse_down`
        // listener did not re-arm `pending_focus_restore`, so a
        // focus-tracking occupant docked instead of tiled would leave
        // shell chords dead after a click).
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-{");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut cx);
        let tile = shell.read_with(&cx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        // `debug_bounds` takes `&'static str`; leak the dynamic selector
        // (test-only, a few bytes).
        let selector: &'static str = Box::leak(format!("tile-content-{}", tile.0).into_boxed_str());
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let focused = shell.read_with(&cx, |s, _| s.focus_handle.clone());
        assert!(
            cx.update(|window, _| focused.is_focused(window)),
            "the shell root has focus again"
        );
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
                .tree()
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
            shell.services.workspaces.active().tree().tiles().len()
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

        let right_tile = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });

        cx.simulate_keystrokes("alt-h");
        let after_left = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
        assert_ne!(
            after_left, right_tile,
            "mod+h (workspace::focus_left) should have moved focus off the right \
             tile"
        );

        cx.simulate_keystrokes("alt-l");
        let after_right = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
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
            let tree = shell.services.workspaces.active().tree();
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

        let before_focus = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });

        // Same layout math as `Render for ShellView`: the tile area is the
        // viewport minus the toolbar, sidebar, and status bar.
        let (target_id, click_point) = cx.update(|window, cx| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);

            let rects = shell
                .read(cx)
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect {
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

        let after_focus = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
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

    /// End-to-end: `ctrl+alt+right` (`workspace::move_right`, a direct
    /// binding) swaps the focused tile with
    /// its right neighbor, focus following the moved tile.
    #[gpui::test]
    fn ctrl_alt_right_keystroke_swaps_the_focused_tile_with_its_right_neighbor(
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

        let focused = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
        let before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        cx.simulate_keystrokes("ctrl-alt-right");

        let after_focused = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
        let after = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });
        assert_eq!(
            after_focused, focused,
            "move_right keeps focus on the same TileId"
        );
        assert_ne!(
            before, after,
            "ctrl+alt+right should have swapped the two tiles' positions"
        );
    }

    /// End-to-end: `ctrl+w` (`workspace::close_tile`) closes the focused
    /// tile.
    #[gpui::test]
    fn ctrl_w_keystroke_closes_the_focused_tile(cx: &mut gpui::TestAppContext) {
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
                .tree()
                .tiles()
                .len()),
            2
        );

        cx.simulate_keystrokes("ctrl-w");

        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count, 1,
            "ctrl+w (workspace::close_tile) should have closed the focused tile"
        );
    }

    /// Shared scaffolding for the dock e2e tests below (dock-regions task):
    /// open a window over a fresh `ShellView`, draw once so the key
    /// dispatch tree exists, and hand back the visual context plus the
    /// downcast shell entity — the exact setup every other e2e test here
    /// builds inline.
    fn dock_test_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::VisualTestContext, Entity<ShellView>) {
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
        (cx, shell)
    }

    /// End-to-end (drag-splitters task): a real press-drag-release on the
    /// splitter between two tiles resizes the pair proportionally to
    /// where the cursor was dropped, never touches tile focus (the strip
    /// occludes the tile edges it overlaps, so the mouse-down that starts
    /// the drag must NOT fire the tiles' click-to-focus), and dirties the
    /// session exactly once, at mouse-up — not per move. The drop point is
    /// deliberately far off the 8px strip: the moves land on the
    /// full-window drag catcher, which is the whole capture mechanism
    /// under test. Cursor appearance (col-resize) is NOT asserted —
    /// gpui's `TestPlatform` records `set_cursor_style` into a private
    /// field with no accessor at the pinned rev, so there is no honest way
    /// to check it from a test.
    #[gpui::test]
    fn dragging_a_main_tree_splitter_resizes_the_pair_and_dirties_the_session(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);

        // Two tiles side by side, focus moved to the LEFT tile — so if
        // the strip's mouse-down leaked through to the right tile under
        // the boundary, click-to-focus would visibly move focus.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-h");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_some(),
            "the splitter strip should have painted between the two tiles"
        );

        shell.update(&mut cx, |shell, _| shell.session_dirty = false);
        let focus_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });

        // Same chrome-offset math as `Render for ShellView` (and the
        // click-to-focus test above): the divider sits at 50% of the tile
        // area's width, offset by the sidebar/toolbar.
        let (grab, drop) = cx.update(|window, _| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            let mid_y = toolbar_height + content_height / 2.0;
            (
                gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
                gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
            )
        });

        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().focused()
            }),
            focus_before,
            "grabbing the splitter must not change tile focus"
        );

        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        let widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .iter()
                .map(|(_, r)| r.w)
                .collect()
        });
        assert!(
            (widths[0] - 0.25).abs() < 1e-3 && (widths[1] - 0.75).abs() < 1e-3,
            "dropping the divider at 25% should relayout the pair 25/75, got {widths:?}"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "moves alone must not dirty the session — only the release does"
        );

        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "releasing the drag should mark the session dirty (drag-resizes persist)"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "the drag should be over after mouse-up"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().focused()
            }),
            focus_before,
            "a divider drag never changes tile focus"
        );
    }

    /// End-to-end (drag-splitters task): dragging the left dock's frame
    /// edge resizes the dock frame itself, live per move, pinning at
    /// `DOCK_MAX_SIZE` when dragged past the clamp instead of failing —
    /// the same press keeps working after crossing the limit.
    #[gpui::test]
    fn dragging_the_left_dock_edge_resizes_the_dock_and_pins_at_the_clamp(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-["); // show the (empty) left dock
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);

        let (tile_width, mid_y) = cx.update(|window, _| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            (tile_width, toolbar_height + content_height / 2.0)
        });
        let dock_size = |shell: &Entity<ShellView>, cx: &gpui::VisualTestContext| {
            shell.read_with(cx, |shell, _| {
                shell
                    .services
                    .workspaces
                    .active()
                    .docks()
                    .get(crate::tiling::DockSide::Left)
                    .size()
            })
        };
        assert!((dock_size(&shell, &cx) - crate::tiling::DOCK_DEFAULT_SIZE).abs() < 1e-4);

        // Grab the dock's inner edge (at 25% of the content width) and
        // drag it to 40%.
        cx.simulate_mouse_down(
            gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_move(
            gpui::point(px(sidebar::WIDTH + tile_width * 0.4), px(mid_y)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert!(
            (dock_size(&shell, &cx) - 0.40).abs() < 1e-3,
            "dragging the edge to 40% should set the dock size to 0.40, got {}",
            dock_size(&shell, &cx)
        );

        // Keep dragging far past the maximum: the size pins at the clamp.
        cx.simulate_mouse_move(
            gpui::point(px(sidebar::WIDTH + tile_width * 0.9), px(mid_y)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert!(
            (dock_size(&shell, &cx) - crate::tiling::DOCK_MAX_SIZE).abs() < 1e-4,
            "dragging past the clamp should stop at DOCK_MAX_SIZE, got {}",
            dock_size(&shell, &cx)
        );

        cx.simulate_mouse_up(
            gpui::point(px(sidebar::WIDTH + tile_width * 0.9), px(mid_y)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a dock-edge drag should persist like any resize"
        );
        assert!(
            (dock_size(&shell, &cx) - crate::tiling::DOCK_MAX_SIZE).abs() < 1e-4,
            "the release must not move the edge again"
        );
    }

    /// Drag-splitters task: fullscreen already suppresses docks and tile
    /// chrome, and the divider strips must follow — `mod+f` (alt+f here,
    /// the test mod alias) makes the strips disappear and a second toggle
    /// brings them back. Asserted via `debug_bounds` (presence of the
    /// painted strip element), the same honest limitation as the hint
    /// tests above.
    #[gpui::test]
    fn fullscreen_suppresses_divider_strips(cx: &mut gpui::TestAppContext) {
        let (mut cx, _shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_some(),
            "two tiles paint their splitter strip"
        );

        cx.simulate_keystrokes("alt-f");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_none(),
            "a fullscreen tile has no visible boundaries — no strips"
        );

        cx.simulate_keystrokes("alt-f");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_some(),
            "leaving fullscreen brings the strips back"
        );
    }

    /// Review fix 1, end-to-end: the keyboard stays live during a drag,
    /// so `mod+2` mid-drag switches workspaces — the drag must cancel
    /// (the recorded address and bounds belong to workspace 1), and a
    /// continued mouse-move must NOT resize workspace 2's tree even
    /// though the same address is structurally valid there. Both
    /// workspaces are set up with the identical two-tile layout precisely
    /// so a wrongly-retargeted move WOULD visibly change workspace 2.
    #[gpui::test]
    fn switching_workspaces_mid_drag_cancels_the_drag_without_retargeting(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);

        // Workspace 1: two tiles. Workspace 2: two tiles, same layout.
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-2");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-1");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (grab, drop_a, drop_b) = cx.update(|window, _| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            let mid_y = toolbar_height + content_height / 2.0;
            (
                gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
                gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
                gpui::point(px(sidebar::WIDTH + tile_width * 0.3), px(mid_y)),
            )
        });

        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(drop_a, MouseButton::Left, gpui::Modifiers::none());

        // Switch to workspace 2 with the button still down, then keep
        // moving.
        cx.simulate_keystrokes("alt-2");
        let ws2_before: Vec<_> = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });
        cx.simulate_mouse_move(drop_b, MouseButton::Left, gpui::Modifiers::none());

        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "the workspace switch should have cancelled the drag"
        );
        let ws2_after: Vec<_> = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });
        assert_eq!(
            ws2_before, ws2_after,
            "the continued move must not resize workspace 2's tree"
        );
        // Workspace 1 keeps the part of the drag that was applied before
        // the switch (cancel is not undo), and — review fix 2 — that
        // applied resize persists: the cancel dirtied the session.
        cx.simulate_keystrokes("alt-1");
        let ws1_widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .iter()
                .map(|(_, r)| r.w)
                .collect()
        });
        assert!(
            (ws1_widths[0] - 0.25).abs() < 1e-3,
            "workspace 1 keeps the applied resize, got {ws1_widths:?}"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a cancelled drag that had moved must still persist its resize"
        );
    }

    /// Review fix 2, end-to-end: opening the palette mid-drag cancels the
    /// drag but keeps — and persists — what it already applied. The first
    /// cut dropped the drag without dirtying the session, so the visible
    /// resize silently diverged from the next restore.
    #[gpui::test]
    fn opening_the_palette_mid_drag_keeps_and_persists_the_applied_resize(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);

        let (grab, drop) = cx.update(|window, _| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            let mid_y = toolbar_height + content_height / 2.0;
            (
                gpui::point(px(sidebar::WIDTH + tile_width * 0.5), px(mid_y)),
                gpui::point(px(sidebar::WIDTH + tile_width * 0.25), px(mid_y)),
            )
        });

        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "mid-drag, nothing is persisted yet"
        );

        cx.simulate_keystrokes("ctrl-k"); // open the palette mid-drag

        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "opening the palette should cancel the drag"
        );
        let widths: Vec<f32> = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .iter()
                .map(|(_, r)| r.w)
                .collect()
        });
        assert!(
            (widths[0] - 0.25).abs() < 1e-3,
            "cancel keeps the applied resize (it is not an undo), got {widths:?}"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "the applied resize must persist even though the drag was cancelled"
        );
    }

    // --- mod+drag tile movement (tile-drag task) ------------------------

    /// The default mod alias (Alt) held on a mouse event — matches the
    /// `alt-h`-style keystrokes the e2e tests already use for `mod+`.
    fn alt_held() -> gpui::Modifiers {
        gpui::Modifiers {
            alt: true,
            ..gpui::Modifiers::none()
        }
    }

    /// Window-space point at fractional coordinates within a main-tree
    /// tile's laid-out rect — the same chrome-offset + dock-carve math
    /// `Render for ShellView` uses, so the tests track real geometry
    /// instead of duplicating guesses.
    fn main_tile_point(
        cx: &mut gpui::VisualTestContext,
        shell: &Entity<ShellView>,
        id: TileId,
        fx: f32,
        fy: f32,
    ) -> gpui::Point<gpui::Pixels> {
        cx.update(|window, app| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let area = Rect {
                x: 0.0,
                y: 0.0,
                w: (f32::from(viewport.width) - sidebar::WIDTH).max(0.0),
                h: (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0),
            };
            let shell = shell.read(app);
            let workspace = shell.services.workspaces.active();
            let (tree_area, _) = crate::tiling::dock_layout(workspace.docks(), area);
            let r = workspace
                .tree()
                .layout(tree_area)
                .into_iter()
                .find(|(t, _)| *t == id)
                .expect("tile present in the main layout")
                .1;
            gpui::point(
                px(sidebar::WIDTH + r.x + r.w * fx),
                px(toolbar_height + r.y + r.h * fy),
            )
        })
    }

    /// Window-space point at fractional coordinates within a visible
    /// dock's frame rect (same math as [`main_tile_point`]).
    fn dock_point(
        cx: &mut gpui::VisualTestContext,
        shell: &Entity<ShellView>,
        side: DockSide,
        fx: f32,
        fy: f32,
    ) -> gpui::Point<gpui::Pixels> {
        cx.update(|window, app| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let area = Rect {
                x: 0.0,
                y: 0.0,
                w: (f32::from(viewport.width) - sidebar::WIDTH).max(0.0),
                h: (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0),
            };
            let shell = shell.read(app);
            let workspace = shell.services.workspaces.active();
            let (_, dock_rects) = crate::tiling::dock_layout(workspace.docks(), area);
            let r = dock_rects
                .into_iter()
                .find(|(s, _)| *s == side)
                .expect("dock visible in the layout")
                .1;
            gpui::point(
                px(sidebar::WIDTH + r.x + r.w * fx),
                px(toolbar_height + r.y + r.h * fy),
            )
        })
    }

    /// Shared setup for the tile-drag e2e tests: two tiles side by side,
    /// focus moved to the LEFT tile, session dirt reset. Returns
    /// `(cx, shell, left, right)`.
    fn two_tile_drag_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::VisualTestContext, Entity<ShellView>, TileId, TileId) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("alt-h"); // focus the left tile
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);
        let tiles: Vec<TileId> = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles()
        });
        let (left, right) = (tiles[0], tiles[1]);
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(left),
            "sanity: focus starts on the left tile"
        );
        (cx, shell, left, right)
    }

    /// End-to-end: a real mod+press on a tile body, dragged past the
    /// movement threshold onto another tile's LEFT edge band and
    /// released, split-inserts the dragged tile on that side — focus
    /// follows the moved tile, the session goes dirty, and the drag is
    /// over. Also pins three recorded decisions along the way: the
    /// mod+down itself must NOT change focus at arm time; the mod key
    /// does not need to stay held once armed (the move and release are
    /// sent with no modifiers); and mid-drag the ghost + zone highlight
    /// paint (via `debug_bounds`, the honest painted-or-not hook).
    #[gpui::test]
    fn mod_dragging_a_tile_onto_anothers_edge_moves_it_and_focus_follows(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(left),
            "mod+down must not change focus at arm time"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()),
            "mod+down on a tile body arms a pending drag"
        );

        // Deep in the left tile's LEFT band, far past the 5px threshold.
        // Modifiers deliberately released: the mod key only gates arming.
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("tile-drag-ghost").is_some(),
            "an active drag paints its cursor ghost"
        );
        assert!(
            cx.debug_bounds("tile-drop-highlight").is_some(),
            "an active drag over a target paints the zone highlight"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "nothing is applied (or persisted) until the drop"
        );

        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .tiles()),
            vec![right, left],
            "an edge drop on the left band inserts the dragged tile before the target"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(right),
            "focus follows the moved tile"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "an applied drop dirties the session"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "the drag is over after the drop"
        );
    }

    /// End-to-end: a sloppy mod+click — press, a 2px wiggle (below the
    /// 5px threshold), release — changes nothing at all: layout, focus
    /// (the recorded no-focus-at-arm decision), and session dirt are all
    /// exactly as before, and no drag remains armed.
    #[gpui::test]
    fn a_below_threshold_mod_click_changes_nothing_at_all(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let wiggle = gpui::point(grab.x + px(2.0), grab.y + px(2.0));
        cx.simulate_mouse_move(wiggle, MouseButton::Left, alt_held());
        cx.simulate_mouse_up(wiggle, MouseButton::Left, alt_held());

        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "a below-threshold mod+click must never rearrange"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(left),
            "focus is untouched — the abandoned gesture leaves everything alone"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "nothing changed, so nothing is persisted"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "the pending drag is cleared on release"
        );
    }

    /// End-to-end: a center drop swaps the two tiles in place (today's
    /// recorded keyboard-parity semantics), focus following the dragged
    /// tile into its new slot.
    #[gpui::test]
    fn mod_dragging_onto_a_tiles_center_swaps_the_pair(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

        let grab = main_tile_point(&mut cx, &shell, left, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let drop = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());

        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .tiles()),
            vec![right, left],
            "a center drop swaps the two tiles"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(left),
            "focus follows the dragged tile to its new slot"
        );
        assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
    }

    /// End-to-end: dropping a tile on a visible (empty) dock's background
    /// inserts it into that dock's tree with the keyboard `dock::move_*`
    /// convention, region and focus following it into the dock.
    #[gpui::test]
    fn mod_dragging_onto_a_dock_background_inserts_into_the_dock(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
        cx.simulate_keystrokes("ctrl-["); // show the (empty) left dock
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let drop = dock_point(&mut cx, &shell, DockSide::Left, 0.5, 0.5);
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());

        shell.read_with(&cx, |shell, _| {
            let workspace = shell.services.workspaces.active();
            assert_eq!(
                workspace.docks().get(DockSide::Left).tree().tiles(),
                vec![right],
                "the dropped tile joins the dock's tree"
            );
            assert_eq!(
                workspace.region(),
                crate::tiling::FocusRegion::Dock(DockSide::Left),
                "the region follows the moved tile into the dock"
            );
            assert!(
                !workspace.tree().contains(right),
                "the tile left the main tree"
            );
        });
        assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
    }

    /// End-to-end cancel guard: `mod+2` switching workspaces mid-drag
    /// cancels the drag cleanly — nothing applied, nothing persisted, the
    /// original workspace's layout untouched when the (now targetless)
    /// release lands.
    #[gpui::test]
    fn switching_workspaces_mid_tile_drag_cancels_with_nothing_applied(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

        cx.simulate_keystrokes("alt-2"); // keyboard stays live mid-drag
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "a workspace switch mid-drag cancels the tile drag"
        );

        cx.simulate_keystrokes("alt-1");
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);
        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "nothing was applied by the cancelled drag"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a cancelled tile drag persists nothing (cancel is truly free)"
        );
    }

    /// End-to-end cancel guard: opening the palette mid-drag (`ctrl+k`)
    /// cancels the tile drag with nothing applied — unlike the divider
    /// drag's palette cancel, which keeps its already-applied live
    /// resize, a tile drag has applied nothing to keep.
    #[gpui::test]
    fn opening_the_palette_mid_tile_drag_cancels_with_nothing_applied(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

        cx.simulate_keystrokes("ctrl-k");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "opening the palette mid-drag cancels the tile drag"
        );

        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "the release after the cancel applies nothing"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "nothing persisted"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "the palette itself stays open (the release is not a dismissing click)"
        );
    }

    /// End-to-end: a plain (no-mod) click on a tile still focuses it and
    /// never arms a drag — the tile-drag feature leaves click-to-focus
    /// byte-for-byte in behavior.
    #[gpui::test]
    fn a_plain_click_still_focuses_and_never_arms_a_drag(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(left)
        );
        let click = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(click, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(right),
            "plain click-to-focus is unchanged"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "no drag arms without the mod key"
        );
    }

    /// Review blocker regression: a keystroke mid-drag flips gpui's
    /// input modality to Keyboard, `MouseUp` does not flip it back, and
    /// `HitboxId::is_hovered` is false under keyboard modality — so a
    /// stationary release after ANY keypress reaches the catcher through
    /// `on_mouse_up_out`, not `on_mouse_up`. The keyboard is documented
    /// hot mid-drag, so that release must still DROP (the fix routes
    /// `up_out` through `finish_tile_drag`); before the fix it silently
    /// cancelled.
    #[gpui::test]
    fn a_keystroke_mid_drag_does_not_turn_a_stationary_release_into_a_cancel(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

        // An unbound key — hits the matcher, matches nothing, changes no
        // shell state, but flips the window's input modality to Keyboard.
        cx.simulate_keystrokes("x");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .tile_drag
                .as_ref()
                .is_some_and(|drag| drag.active)),
            "an unbound keystroke mid-drag must not cancel the drag"
        );

        // Release without moving: under keyboard modality this dispatches
        // through the catcher's `on_mouse_up_out` gate.
        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .tiles()),
            vec![right, left],
            "the stationary release after a keystroke must still apply the edge drop"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(right),
            "focus follows the moved tile"
        );
        assert!(shell.read_with(&cx, |shell, _| shell.session_dirty));
        assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()));
    }

    /// Review should-fix regression: in production, input events arrive
    /// between frames — the palette-toggle keystroke and the release can
    /// both land before any render runs the cancel guard (the test
    /// harness draws at the end of every simulated event's update, so
    /// the two events are dispatched inside ONE `cx.update` here, the
    /// same one-frame window real platforms produce; the mid-update
    /// asserts verify the guard genuinely hasn't run). The drop-time
    /// re-check in `finish_tile_drag` must refuse to apply the drop
    /// underneath the just-opened palette.
    #[gpui::test]
    fn a_release_in_the_same_frame_as_the_palette_opening_applies_nothing(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

        cx.update(|window, cx| {
            window.dispatch_keystroke(gpui::Keystroke::parse("ctrl-k").unwrap(), cx);
            assert!(
                shell.read(cx).palette.is_some(),
                "the keystroke opened the palette"
            );
            assert!(
                shell.read(cx).tile_drag.is_some(),
                "no render has run since the keystroke, so the render-top guard has \
                 not cancelled the drag — the drop-time re-check is the only defense"
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: drop,
                    modifiers: gpui::Modifiers::none(),
                    click_count: 1,
                }),
                cx,
            );
        });
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "the release must not apply the drop underneath the just-opened palette"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "nothing applied, nothing persisted"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "the drag is over either way"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "the palette stays open"
        );
    }

    /// Post-merge review BUG 1 regression (phantom armed drag): gpui
    /// dispatches multiple input events between frames, so a fast
    /// mod+click can land its mouse-DOWN and mouse-UP inside one frame
    /// window — before any draw registers the tile-drag catcher's up
    /// handlers. Before the fix the armed (never-activated) drag survived
    /// that release forever: the next frame painted the full-window
    /// grabbing catcher, the user's next stationary click was eaten, and
    /// an unmodified press-drag-release could be APPLIED as a
    /// rearrangement without the mod key held. The fix (root-level
    /// mouse-up fallback) must clear the armed drag on that same-frame
    /// release, and a subsequent unmodified press-drag-release must
    /// change nothing.
    #[gpui::test]
    fn a_mod_click_released_in_the_arm_frame_leaves_no_phantom_drag(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        // Down + up dispatched inside ONE `cx.update`, no draw between —
        // the same one-frame window real platforms produce (technique
        // from the same-frame palette test above).
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: grab,
                    modifiers: alt_held(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            assert!(
                shell.read(cx).tile_drag.is_some(),
                "sanity: the mod+down armed a pending drag"
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: grab,
                    modifiers: alt_held(),
                    click_count: 1,
                }),
                cx,
            );
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "a release in the same frame as the arm must clear the armed drag \
             (no phantom drag survives to the catcher's first paint)"
        );

        // An unmodified press-drag(>5px)-release afterwards must behave
        // like the plain gesture it is: click-to-focus, no rearrangement.
        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        let far = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_move(far, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(far, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "an unmodified press-drag-release after the phantom window must not \
             rearrange the layout"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .services
                .workspaces
                .active()
                .tree()
                .focused()),
            Some(right),
            "the plain click focused the tile it landed on (click-to-focus intact)"
        );
    }

    /// Fix-round should-fix (the divider-drag twin of BUG 1): a strip's
    /// mouse-down arms `divider_drag`, but the divider catcher's up
    /// handlers only enter the hitbox tree at the next paint — so a
    /// sub-frame click on a strip (down + up before any draw) left a
    /// phantom armed divider drag: the full-window resize-cursor catcher
    /// painted, the next mouse-down was swallowed, and an unmodified
    /// press-drag (no intervening buttonless move) live-RESIZED the
    /// phantom's divider. The root-element release fallback must clear
    /// it, and a subsequent unmodified press-drag must resize nothing.
    #[gpui::test]
    fn a_strip_click_released_in_the_arm_frame_leaves_no_phantom_divider_drag(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
        let widths_before: Vec<f32> = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .into_iter()
                .map(|(_, r)| r.w)
                .collect()
        });

        let strip = cx
            .debug_bounds("divider-strip-0")
            .expect("two tiles paint their splitter strip");
        let grab = strip.center();
        // Down + up dispatched inside ONE `cx.update`, no draw between —
        // the same technique as the tile-drag phantom test above.
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: grab,
                    modifiers: gpui::Modifiers::none(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            assert!(
                shell.read(cx).divider_drag.is_some(),
                "sanity: the strip's mouse-down armed a divider drag"
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: grab,
                    modifiers: gpui::Modifiers::none(),
                    click_count: 1,
                }),
                cx,
            );
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "a release in the same frame as the arm must clear the armed \
             divider drag (no phantom survives to the catcher's first paint)"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a phantom that moved nothing persists nothing"
        );

        // An unmodified press-drag afterwards must be the plain gesture it
        // is (click-to-focus on the tile it lands on), never a live resize
        // of the phantom's divider.
        let press = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(press, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_move(
            gpui::point(press.x - px(120.0), press.y),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(
            gpui::point(press.x - px(120.0), press.y),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .layout(Rect::UNIT)
                    .into_iter()
                    .map(|(_, r)| r.w)
                    .collect::<Vec<f32>>()
            }),
            widths_before,
            "an unmodified press-drag after the phantom window must not resize \
             any divider"
        );
    }

    /// Post-merge review BUG 2: Escape mid-tile-drag cancels the drag —
    /// nothing applied when the (now targetless) release lands, nothing
    /// persisted, and the keystroke never reaches the matcher.
    #[gpui::test]
    fn escape_mid_tile_drag_cancels_with_nothing_applied(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .tile_drag
                .as_ref()
                .is_some_and(|drag| drag.active)),
            "sanity: the drag is active before Escape"
        );

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "Escape mid-drag must cancel the tile drag"
        );

        cx.simulate_mouse_up(drop, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "the release after the Escape cancel applies nothing"
        );
        assert!(
            !shell.read_with(&cx, |shell, _| shell.session_dirty),
            "a cancelled tile drag persists nothing"
        );
    }

    /// Post-merge review BUG 2 (armed-but-inactive arm): Escape also
    /// clears a drag that never crossed the movement threshold, so the
    /// release afterwards is a plain unarmed release.
    #[gpui::test]
    fn escape_clears_an_armed_but_inactive_tile_drag(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, _left, right) = two_tile_drag_shell(cx);
        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()));

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "Escape must clear an armed-but-inactive drag too"
        );
    }

    /// Post-merge review BUG 2 (divider consistency, recorded decision):
    /// Escape mid-divider-drag ENDS the drag — finish, not revert,
    /// because a divider drag's resizes were already applied live and
    /// cancel means "stop tracking the mouse", never "undo". Applied
    /// moves persist (session dirty) and further mouse moves resize
    /// nothing.
    #[gpui::test]
    fn escape_mid_divider_drag_finishes_it_keeping_applied_resizes(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, _left, _right) = two_tile_drag_shell(cx);

        // Grab the divider between the two tiles and drag it left.
        let strip = cx
            .debug_bounds("divider-strip-0")
            .expect("two tiles paint their splitter strip");
        let grab = strip.center();
        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));
        let target = gpui::point(grab.x - px(100.0), grab.y);
        cx.simulate_mouse_move(target, MouseButton::Left, gpui::Modifiers::none());
        let widths_after_move: Vec<f32> = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .tree()
                .layout(Rect::UNIT)
                .into_iter()
                .map(|(_, r)| r.w)
                .collect()
        });

        cx.simulate_keystrokes("escape");
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "Escape mid-divider-drag must end the drag"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.session_dirty),
            "the applied resize persists (finish, not revert)"
        );

        // Further moves with the button still (nominally) held must no
        // longer resize anything — the drag is over.
        let farther = gpui::point(grab.x - px(200.0), grab.y);
        cx.simulate_mouse_move(farther, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .layout(Rect::UNIT)
                    .into_iter()
                    .map(|(_, r)| r.w)
                    .collect::<Vec<f32>>()
            }),
            widths_after_move,
            "no further tracking after Escape ended the divider drag"
        );
    }

    /// Post-merge review BUG 3: ctrl+w can close the dragged tile
    /// mid-drag (the keyboard stays hot), and neither the render-top
    /// guard nor `finish_tile_drag` checked the tile still exists —
    /// leaving a ghost + zone highlight promising a drop that would
    /// silently no-op. The dragged tile's existence must join the shared
    /// cancel conditions: the drag cancels at the next paint, no
    /// highlight paints, and the release applies nothing.
    #[gpui::test]
    fn closing_the_dragged_tile_mid_drag_cancels_the_drag(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

        // Drag the LEFT tile (the focused one — ctrl+w closes the focused
        // tile, so dragging it is what makes the close hit the drag).
        let grab = main_tile_point(&mut cx, &shell, left, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let over = main_tile_point(&mut cx, &shell, right, 0.05, 0.5);
        cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("tile-drop-highlight").is_some(),
            "sanity: the active drag paints its zone highlight before the close"
        );

        cx.simulate_keystrokes("ctrl-w"); // closes the focused (= dragged) tile
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "closing the dragged tile mid-drag must cancel the drag"
        );
        assert!(
            cx.debug_bounds("tile-drop-highlight").is_none(),
            "no zone highlight may keep painting for a tile that no longer exists"
        );
        assert!(
            cx.debug_bounds("tile-drag-ghost").is_none(),
            "no ghost may keep painting for a tile that no longer exists"
        );

        shell.update(&mut cx, |shell, _| shell.session_dirty = false);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });
        cx.simulate_mouse_up(over, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "the release after the cancel applies nothing"
        );
        assert!(!shell.read_with(&cx, |shell, _| shell.session_dirty));
    }

    /// Post-merge review BUG 4: platform-uniform chorded-button handling.
    /// macOS delivers a right/middle-dragged event as a MouseMoveEvent
    /// with `pressed_button: Some(Right/Middle)` (gpui_macos events.rs
    /// translates NSRightMouseDragged/NSOtherMouseDragged verbatim, no
    /// left-first normalization), so before the fix a chorded second
    /// button CANCELLED a mid-flight tile drag on macOS while Windows
    /// (whose WM_MOUSEMOVE translation checks MK_LBUTTON first) let it
    /// survive. The unified rule: a non-Left-button move is IGNORED
    /// (neither advances nor cancels); only a buttonless move is the
    /// lost-release cancel.
    #[gpui::test]
    fn a_chorded_second_button_move_mid_drag_neither_cancels_nor_advances(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        let over = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .tile_drag
                .as_ref()
                .is_some_and(|drag| drag.active)),
            "sanity: the drag is active"
        );

        // A chorded right-button move (the macOS NSRightMouseDragged
        // shape) at a DIFFERENT position: the drag must survive AND not
        // track it (ignored entirely).
        let elsewhere = main_tile_point(&mut cx, &shell, right, 0.9, 0.9);
        cx.simulate_mouse_move(elsewhere, MouseButton::Right, gpui::Modifiers::none());
        shell.read_with(&cx, |shell, _| {
            let drag = shell
                .tile_drag
                .as_ref()
                .expect("a chorded second-button move must not cancel the drag");
            assert_eq!(
                drag.cursor,
                (f32::from(over.x), f32::from(over.y)),
                "an ignored move must not advance the drag's cursor either"
            );
        });

        // A buttonless move IS the lost-release signal: cancel.
        cx.simulate_mouse_move(elsewhere, None, gpui::Modifiers::none());
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "a buttonless move (lost release) must cancel the drag"
        );
    }

    /// Post-merge review BUG 4, divider side: the divider catcher had the
    /// same `pressed_button != Some(Left)` branch, so a chorded second
    /// button FINISHED an in-flight divider drag on macOS. Same unified
    /// rule: non-Left moves are ignored, buttonless moves finish.
    #[gpui::test]
    fn a_chorded_second_button_move_mid_divider_drag_does_not_finish_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell, _left, _right) = two_tile_drag_shell(cx);
        let strip = cx
            .debug_bounds("divider-strip-0")
            .expect("two tiles paint their splitter strip");
        let grab = strip.center();
        cx.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::none());
        assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));

        cx.simulate_mouse_move(
            gpui::point(grab.x - px(50.0), grab.y),
            MouseButton::Right,
            gpui::Modifiers::none(),
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()),
            "a chorded second-button move must not finish the divider drag"
        );

        cx.simulate_mouse_move(
            gpui::point(grab.x - px(50.0), grab.y),
            None,
            gpui::Modifiers::none(),
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "a buttonless move (lost release) must finish the divider drag"
        );
    }

    /// Post-merge review finding 6: cmd+tab away with the button held,
    /// release elsewhere — without an activation observer the stale
    /// ACTIVE drag persisted and the re-activation click could advance
    /// and apply it. `ShellView::new` now registers
    /// `cx.observe_window_activation` (verified available at the pinned
    /// gpui rev) and ends both drag kinds on deactivation (tile: cancel;
    /// divider: finish). The test drives the harness's real activation
    /// plumbing: `activate_window` marks the test window active, and
    /// `deactivate_window` fires the platform active-status callback.
    #[gpui::test]
    fn window_deactivation_mid_drag_ends_both_drag_kinds(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();

        // Tile drag: deactivation cancels with nothing applied.
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });
        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        let over = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        cx.simulate_mouse_move(over, MouseButton::Left, gpui::Modifiers::none());
        assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_some()));

        cx.deactivate_window();
        assert!(
            shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()),
            "window deactivation mid-tile-drag must cancel the drag"
        );
        cx.simulate_mouse_up(over, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "the release after re-activation applies nothing"
        );

        // Divider drag: deactivation finishes it (applied moves persist).
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let strip = cx
            .debug_bounds("divider-strip-0")
            .expect("two tiles paint their splitter strip");
        let dgrab = strip.center();
        cx.simulate_mouse_down(dgrab, MouseButton::Left, gpui::Modifiers::none());
        assert!(shell.read_with(&cx, |shell, _| shell.divider_drag.is_some()));
        cx.deactivate_window();
        assert!(
            shell.read_with(&cx, |shell, _| shell.divider_drag.is_none()),
            "window deactivation mid-divider-drag must end the drag"
        );
    }

    /// Post-merge review finding 7 (one-frame workspace ABA): the drop
    /// re-check used to compare workspace INDEX equality only, so a
    /// switch away and back with no render between satisfied the letter
    /// of the check while violating its intent. The switch-epoch pin
    /// closes it: any actual switch bumps the epoch, so away-and-back
    /// can never look like "never left".
    ///
    /// Honesty note on how the state is built: at the pinned gpui rev
    /// this gap is NOT reachable through the real key pipeline — traced
    /// while writing this test: `Window::dispatch_key_event` draws first
    /// whenever the window is dirty, and the first switch's notify makes
    /// it dirty, so the second switch's keystroke always runs the
    /// render-top cancel guard (index mismatch) before dispatching.
    /// `dispatch_mouse_event` does NOT draw-when-dirty, but the only
    /// mouse path to a switch (a sidebar pill click) is occluded by the
    /// drag catcher mid-drag. The epoch re-check is defense in depth for
    /// exactly that reason — it must hold even if gpui's dispatch-order
    /// details change under an upgrade — so the test dispatches the
    /// switch ACTIONS directly (no key dispatch, no draw), constructing
    /// the letter-of-the-rule state the guard can't otherwise see.
    #[gpui::test]
    fn switching_away_and_back_within_one_frame_voids_the_drop(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell, left, right) = two_tile_drag_shell(cx);
        let layout_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().layout(Rect::UNIT)
        });

        let grab = main_tile_point(&mut cx, &shell, right, 0.5, 0.5);
        let drop = main_tile_point(&mut cx, &shell, left, 0.05, 0.5);
        cx.simulate_mouse_down(grab, MouseButton::Left, alt_held());
        cx.simulate_mouse_move(drop, MouseButton::Left, gpui::Modifiers::none());

        // Switch away, switch back, and release — all inside ONE
        // `cx.update`, no draw between: the index is back to where the
        // drag started by release time, so only the epoch comparison can
        // refuse the drop.
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(
                    &ActionId("workspace::switch_2".to_string()),
                    None,
                    window,
                    cx,
                );
                shell.dispatch(
                    &ActionId("workspace::switch_1".to_string()),
                    None,
                    window,
                    cx,
                );
            });
            assert_eq!(
                shell.read(cx).services.workspaces.active_index(),
                1,
                "sanity: back on the original workspace before the release"
            );
            assert!(
                shell.read(cx).tile_drag.is_some(),
                "no render has run, so the render-top guard has not cancelled \
                 the drag — the drop-time epoch re-check is the only defense"
            );
            window.dispatch_event(
                gpui::PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: drop,
                    modifiers: gpui::Modifiers::none(),
                    click_count: 1,
                }),
                cx,
            );
        });
        assert_eq!(
            shell.read_with(&cx, |shell, _| {
                shell.services.workspaces.active().tree().layout(Rect::UNIT)
            }),
            layout_before,
            "a release after an away-and-back switch inside one frame must \
             apply nothing"
        );
        assert!(shell.read_with(&cx, |shell, _| shell.tile_drag.is_none()));
    }

    /// Review fix 4: the which-key hint paints a solid panel with no
    /// occlusion and no handlers, so a mouse-down through it would fall
    /// onto a strip beneath — a pending keystroke sequence must therefore
    /// gate the strips off exactly like the palette/modal overlays do,
    /// and completing the sequence brings them back.
    #[gpui::test]
    fn a_pending_key_sequence_gates_the_divider_strips(cx: &mut gpui::TestAppContext) {
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

        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_some(),
            "two tiles paint their splitter strip"
        );

        cx.simulate_keystrokes("g"); // first key of the "g g" sequence
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("whichkey-overlay").is_some(),
            "sanity: the which-key overlay is up while the sequence is pending"
        );
        assert!(
            cx.debug_bounds("divider-strip-0").is_none(),
            "a pending sequence (which-key showing) must gate the strips off"
        );

        cx.simulate_keystrokes("g"); // completes the sequence
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("divider-strip-0").is_some(),
            "resolving the sequence brings the strips back"
        );
    }

    /// End-to-end: `ctrl+[` (`dock::toggle_left`) through gpui's real key
    /// pipeline toggles the left dock's visibility both ways, and the
    /// visible-but-empty dock paints its "move a tile here" hint (asserted
    /// via `debug_bounds`, same honest limitation as the empty-workspace
    /// hint test above — text content itself can't be inspected).
    #[gpui::test]
    fn ctrl_bracket_keystroke_toggles_the_left_dock_and_paints_its_hint(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);

        cx.simulate_keystrokes("ctrl-v"); // one tile so the workspace isn't bare
        cx.simulate_keystrokes("ctrl-[");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (visible, region) = shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            (
                ws.docks().get(crate::tiling::DockSide::Left).visible(),
                ws.region(),
            )
        });
        assert!(visible, "ctrl+[ should have shown the left dock");
        assert_eq!(
            region,
            crate::tiling::FocusRegion::Main,
            "showing an empty dock must not move focus into it"
        );

        let hint_bounds = cx.debug_bounds("dock-empty-hint-left");
        assert!(
            hint_bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the empty left dock should have painted its hint, got {hint_bounds:?}"
        );

        cx.simulate_keystrokes("ctrl-[");
        let visible = shell.read_with(&cx, |shell, _| {
            shell
                .services
                .workspaces
                .active()
                .docks()
                .get(crate::tiling::DockSide::Left)
                .visible()
        });
        assert!(
            !visible,
            "a second ctrl+[ should have hidden the dock again"
        );
    }

    /// End-to-end: the move-to-dock chord. The user presses ctrl+shift+[,
    /// but both real platforms deliver that as key `{` with the shift
    /// modifier CLEARED (see BUILTIN_KEYMAP's doc comment for the verified
    /// platform-source evidence), so the simulated keystroke is `ctrl-{` —
    /// which gpui's test parser produces in exactly that platform shape
    /// (key `{`, no shift). This test pins that the `"ctrl+{"` binding
    /// matches it end to end: the focused tile leaves the tree, parks in
    /// the left dock, focus follows, and the session goes dirty. A second
    /// ctrl+{ sends it back into the tree and auto-hides the dock.
    #[gpui::test]
    fn ctrl_brace_keystroke_moves_the_tile_to_the_left_dock_and_back(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut cx, shell) = dock_test_shell(cx);

        cx.simulate_keystrokes("ctrl-v");
        let tile = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused().unwrap()
        });
        // Clear the dirty flag left by the split so the assertion below
        // isolates the dock move's own dirtying.
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);

        cx.simulate_keystrokes("ctrl-{");

        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            assert!(ws.tree().is_empty(), "the tile should have left the tree");
            let dock = ws.docks().get(crate::tiling::DockSide::Left);
            assert_eq!(dock.tree().tiles(), vec![tile]);
            assert!(dock.visible(), "the dock auto-shows");
            assert_eq!(
                ws.region(),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
            );
            assert!(
                shell.session_dirty,
                "a handled dock action must mark the session dirty"
            );
        });

        cx.simulate_keystrokes("ctrl-{");

        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            assert_eq!(ws.tree().tiles(), vec![tile], "the tile returned");
            assert_eq!(ws.tree().focused(), Some(tile));
            assert_eq!(ws.region(), crate::tiling::FocusRegion::Main);
            let dock = ws.docks().get(crate::tiling::DockSide::Left);
            assert!(dock.tree().is_empty());
            assert!(!dock.visible(), "the emptied dock auto-hides");
        });
    }

    /// End-to-end (dock-trees task): splits work *inside* a focused dock
    /// through gpui's real key pipeline. ctrl+v parks a tile via ctrl+{,
    /// then a second ctrl+v splits within the dock's tree (the old
    /// build refused this) — two tiles in the dock, session dirty — and
    /// ctrl+w closes one, leaving the dock visible with the
    /// survivor.
    #[gpui::test]
    fn splits_and_close_operate_inside_a_focused_dock(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell) = dock_test_shell(cx);

        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-{"); // tile → left dock, dock focused
        shell.update(&mut cx, |shell, _| shell.session_dirty = false);

        cx.simulate_keystrokes("ctrl-v"); // split inside the dock
        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            let dock = ws.docks().get(crate::tiling::DockSide::Left);
            assert_eq!(
                dock.tree().tiles().len(),
                2,
                "ctrl+v must split within the focused dock's tree"
            );
            assert_eq!(
                ws.region(),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
            );
            assert!(ws.tree().is_empty(), "the main tree must stay untouched");
            assert!(
                shell.session_dirty,
                "a dock-tree split must mark the session dirty"
            );
        });

        cx.simulate_keystrokes("ctrl-w"); // close the focused dock tile
        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            let dock = ws.docks().get(crate::tiling::DockSide::Left);
            assert_eq!(dock.tree().tiles().len(), 1);
            assert!(dock.visible(), "a still-occupied dock must not auto-hide");
            assert_eq!(
                ws.region(),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left),
                "focus stays in the dock while it has tiles"
            );
        });
    }

    /// End-to-end: a literal shift-held `[` must NOT trigger the move
    /// binding — the platforms never deliver that shape (they deliver
    /// `{`), and gpui's test dispatcher faithfully reproduces whatever
    /// shape it's given, so this pins that the binding was NOT written as
    /// `"ctrl+shift+["` (which would match only this never-occurring
    /// event and nothing real).
    #[gpui::test]
    fn a_literal_ctrl_shift_bracket_shape_does_not_move_the_tile(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-shift-["); // key "[", shift=true: not a real platform shape
        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            assert!(
                !ws.tree().is_empty(),
                "the unmatched keystroke must not have moved the tile"
            );
            assert!(
                ws.docks()
                    .get(crate::tiling::DockSide::Left)
                    .tree()
                    .is_empty()
            );
        });
    }

    /// Review nit (re-grounded for dock-trees): with the tree empty and
    /// the workspace's only tile parked in a focused dock, the tree area
    /// must NOT show the "ctrl+h / ctrl+v to open a tile" hint — a
    /// dock-focused split now lands in the *dock's* tree, so that advice
    /// would not fill the empty main area. It shows the move-back hint for
    /// the focused dock instead (physical-key spelling, like the dock
    /// hints). Same `debug_bounds` honesty limits as the other hint tests:
    /// selectors, not text.
    #[gpui::test]
    fn empty_tree_hint_is_state_aware_while_a_dock_holds_focus(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-{"); // only tile → left dock, tree empty, dock focused
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        shell.read_with(&cx, |shell, _| {
            let ws = shell.services.workspaces.active();
            assert!(ws.tree().is_empty(), "sanity: the tree emptied");
            assert_eq!(
                ws.region(),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left)
            );
        });

        let return_hint = cx.debug_bounds("empty-hint-return-left");
        assert!(
            return_hint.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the dock-focused empty tree should paint the move-back hint, got {return_hint:?}"
        );
        assert_eq!(
            cx.debug_bounds("empty-hint"),
            None,
            "the split hint must not paint while a dock holds focus (a split lands in the dock)"
        );

        // Back in Main over the still-empty tree, the ordinary split hint
        // returns (region falls back to the dock being the only occupant —
        // so go through move-back, then close, leaving a truly empty
        // Main-focused workspace).
        cx.simulate_keystrokes("ctrl-{"); // tile returns to the tree
        cx.simulate_keystrokes("ctrl-w"); // close it: empty workspace, Main
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let split_hint = cx.debug_bounds("empty-hint");
        assert!(
            split_hint.is_some_and(|b| b.size.width > px(0.0)),
            "with Main focused the ordinary split hint returns, got {split_hint:?}"
        );
    }

    /// End-to-end geometry: with a tile parked in the left dock and one in
    /// the tree, the surface carves the dock column out of the tree's area
    /// — the tree's layout (the same call `render` makes) starts at the
    /// dock's right edge, and the whole pass still calls `Tree::layout`
    /// once (structural: this asserts the observable carve-up, the
    /// call-count discipline is by construction in `render`).
    #[gpui::test]
    fn a_visible_left_dock_carves_its_column_out_of_the_tree_area(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell) = dock_test_shell(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-v");
        cx.simulate_keystrokes("ctrl-{"); // right tile → left dock
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|window, cx| {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let tile_width = (f32::from(viewport.width) - sidebar::WIDTH).max(0.0);
            let content_height =
                (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0);
            let area = Rect {
                x: 0.0,
                y: 0.0,
                w: tile_width,
                h: content_height,
            };
            let shell = shell.read(cx);
            let ws = shell.services.workspaces.active();
            let (tree_area, dock_rects) = crate::tiling::dock_layout(ws.docks(), area);
            assert_eq!(dock_rects.len(), 1);
            let (side, dock_rect) = dock_rects[0];
            assert_eq!(side, crate::tiling::DockSide::Left);
            let expected_w = crate::tiling::DOCK_DEFAULT_SIZE * tile_width;
            assert!(
                (dock_rect.w - expected_w).abs() < 1e-3,
                "dock width {} should be size*area_width {}",
                dock_rect.w,
                expected_w
            );
            assert!((dock_rect.h - content_height).abs() < 1e-3, "full height");
            let rects = ws.tree().layout(tree_area);
            assert_eq!(rects.len(), 1);
            let tree_tile = rects[0].1;
            assert!(
                (tree_tile.x - dock_rect.w).abs() < 1e-3,
                "the tree starts where the dock column ends: {} vs {}",
                tree_tile.x,
                dock_rect.w
            );
            assert!(
                (tree_tile.w - (tile_width - dock_rect.w)).abs() < 1e-3,
                "the tree gets the rest of the width"
            );
        });
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
    /// direct to `ctrl+alt+arrows`), so this isolated binding is the way
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
            roster: crate::module::ModuleRoster::default(),
            restored_tiles: crate::session::TileRecords::new(),
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
            shell.services.workspaces.active().tree().tiles().len()
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
    /// leave any trace once the palette is dismissed. Also covers the
    /// palette-input-polish task's focus contract: `ctrl+k` should have
    /// focused `palette_input`'s real `FocusHandle` (proven directly, not
    /// just inferred from typing having worked), and escape should hand
    /// focus back to the shell root — the same "return focus on close"
    /// story `escape_in_the_filter_input_returns_focus_to_the_shell_root`
    /// proves for the toolbar's filter field.
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
        let shell_focus_handle = shell.read_with(&cx, |shell, _| shell.focus_handle.clone());
        let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());
        let palette_input_focus_handle =
            palette_input.read_with(&cx, |state, cx| state.focus_handle(cx));

        cx.simulate_keystrokes("ctrl-k");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
            "ctrl+k opening the palette should have focused its query Input"
        );

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
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "escape must not dispatch the item that was filtered/selected"
        );
        assert!(
            !cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
            "escape should have moved focus off the palette's query input"
        );
        assert!(
            cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "escape should have returned focus to the shell root"
        );
    }

    /// Left/right arrow keys are consumed by the palette's query `Input` as
    /// native caret movement (palette-input-polish task: "OS text input
    /// stuff... from the component") and must not leak to the shell as
    /// workspace chords — proven two ways: the caret actually moves inside
    /// the input (`InputState::cursor`, not inferred from the query
    /// staying the same), and the workspace stays untouched.
    #[gpui::test]
    fn left_and_right_arrows_move_the_input_caret_and_do_not_leak_to_the_shell(
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
        let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());

        cx.simulate_keystrokes("ctrl-k");
        cx.simulate_input("abc");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            palette_input.read_with(&cx, |state, _cx| state.cursor()),
            3,
            "sanity: typing \"abc\" should leave the caret at the end"
        );

        cx.simulate_keystrokes("left");
        assert_eq!(
            palette_input.read_with(&cx, |state, _cx| state.cursor()),
            2,
            "left should move the caret back one position inside the input"
        );

        cx.simulate_keystrokes("right");
        assert_eq!(
            palette_input.read_with(&cx, |state, _cx| state.cursor()),
            3,
            "right should move the caret forward one position inside the input"
        );

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "the palette should still be open — arrows are caret movement, not close"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "left/right must not leak to the shell as workspace chords"
        );
    }

    /// ctrl+a is consumed by the palette's query `Input` (native "OS text
    /// input stuff") rather than leaking to the shell — there is no
    /// `ctrl+a` shell binding at all (checked against `defaults.rs`'s
    /// `BUILTIN_KEYMAP`), so the meaningful proof is that the input
    /// actually reacts to it and the query/palette are otherwise
    /// untouched. Platform quirk, asserted directly rather than assumed
    /// (gpui-component's own hardcoded bindings, `crates/base/src/input/
    /// base/state.rs`, not this crate's configurable mod-alias): on macOS
    /// `ctrl+a` is bound to `MoveHome` (Emacs-style — `cmd+a` is
    /// `SelectAll` there instead), everywhere else `ctrl+a` *is*
    /// `SelectAll`. Both handlers fully consume the keystroke (neither
    /// calls `cx.propagate()` — checked against the pinned checkout), so
    /// "does not leak" holds on every platform CI builds this on (spec: “CI
    /// runs on both macOS and Windows”); only the resulting caret/selection
    /// differs.
    #[gpui::test]
    fn ctrl_a_is_consumed_by_the_input_and_does_not_leak_to_the_shell(
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
        let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());

        cx.simulate_keystrokes("ctrl-k");
        cx.simulate_input("split");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("ctrl-a");

        #[cfg(target_os = "macos")]
        assert_eq!(
            palette_input.read_with(&cx, |state, _cx| state.cursor()),
            0,
            "on macOS, ctrl+a inside a gpui-component Input is MoveHome, not SelectAll"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            palette_input.read_with(&cx, |state, _cx| state.selected_range()),
            0..5,
            "ctrl+a should select the whole \"split\" query inside the input"
        );

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "ctrl+a must not close the palette"
        );
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .palette
                .as_ref()
                .unwrap()
                .query()
                .to_string()),
            "split",
            "ctrl+a must not itself change the query text"
        );
    }

    /// A row click SELECTS it (moves the highlight) without dispatching —
    /// Enter is still what dispatches. Real mouse coordinates, recovered
    /// from `palette::render`'s `"palette-row-{i}"` debug selector (same
    /// pattern `arrow_down_past_visible_rows_advances_selection_and_
    /// scrolls_it_into_view` and `keybindings_view`'s own row-click test
    /// use) rather than a direct `PaletteState::set_selected` call, so this
    /// exercises the real click -> `ShellView::render`'s `on_row_click` ->
    /// `set_selected` path end to end.
    #[gpui::test]
    fn click_on_a_result_row_selects_it_without_dispatching(cx: &mut gpui::TestAppContext) {
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
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            0,
            "sanity: the palette opens with row 0 selected"
        );

        let row_bounds = cx
            .debug_bounds("palette-row-3")
            .expect("row 3 should have painted bounds to click into");
        let inside_row_3 = gpui::point(
            row_bounds.origin.x + gpui::px(10.0),
            row_bounds.origin.y + gpui::px(10.0),
        );
        cx.simulate_mouse_down(inside_row_3, MouseButton::Left, gpui::Modifiers::none());

        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            3,
            "clicking row 3 should select it"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "a row click must not dispatch — the palette stays open"
        );
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count, 0,
            "selecting a row via click must not have dispatched anything"
        );
    }

    /// A mouse-down well outside the palette panel — on the transparent
    /// click-catcher `ShellView::render` wraps the panel in — dismisses the
    /// palette (design brief: "click anywhere outside the palette panel ->
    /// dismisses the palette"). Same real-mouse-event structure and corner
    /// point as `backdrop_click_closes_the_modal` (the panel is centered,
    /// starting at least a third of the way down and inset horizontally,
    /// so a point near the window's origin always falls on the catcher).
    #[gpui::test]
    fn click_outside_the_palette_panel_closes_it(cx: &mut gpui::TestAppContext) {
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
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "sanity: ctrl-k should have opened the palette"
        );

        cx.simulate_mouse_down(
            gpui::point(gpui::px(4.0), gpui::px(4.0)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "a mouse-down on the click-catcher, well outside the centered \
             panel, should have closed the palette"
        );
    }

    /// A mouse-down INSIDE the panel must NOT close the palette — the
    /// panel's own `on_mouse_down` (`palette::render`) stops propagation
    /// before the same bubbling event ever reaches the click-catcher's
    /// close handler underneath it. Mirrors `panel_click_does_not_close_
    /// the_modal` exactly, one layer down (palette panel vs. modal panel).
    #[gpui::test]
    fn click_on_the_palette_panel_does_not_close_it(cx: &mut gpui::TestAppContext) {
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
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "sanity: ctrl-k should have opened the palette"
        );

        let panel_bounds = cx
            .debug_bounds("palette-panel")
            .expect("the palette panel should have painted bounds to click inside");
        let inside_panel = gpui::point(
            panel_bounds.origin.x + gpui::px(10.0),
            panel_bounds.origin.y + gpui::px(10.0),
        );

        cx.simulate_mouse_down(inside_panel, MouseButton::Left, gpui::Modifiers::none());

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "a mouse-down inside the panel must not close the palette"
        );
    }

    /// A shell chord (`ctrl+w` = `workspace::close_tile`) must not
    /// fire while the palette is open — proven with a real tile actually
    /// present to close (an empty workspace closing "a tile" that was
    /// never there wouldn't distinguish "correctly swallowed" from
    /// "there was nothing to close anyway").
    #[gpui::test]
    fn shell_chord_does_not_fire_while_the_palette_is_open(cx: &mut gpui::TestAppContext) {
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

        // Create a real tile (ctrl+v = workspace::split_right) so there is
        // something for a leaked ctrl+w to actually close.
        cx.simulate_keystrokes("ctrl-v");
        let tile_count_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count_before, 1,
            "sanity: ctrl+v should have split a tile"
        );

        cx.simulate_keystrokes("ctrl-k");
        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "sanity: ctrl-k should have opened the palette"
        );

        cx.simulate_keystrokes("ctrl-w");

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_some()),
            "ctrl+w must not close the palette either"
        );
        let tile_count_after = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(
            tile_count_after, 1,
            "ctrl+w (workspace::close_tile) must not fire while the \
             palette is open — the tile from before must still be there"
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

    /// The palette gains the dialogs' larger steps (spec §3): ctrl+d/u
    /// move ±5, ctrl+f/b and pageup/pagedown ±10.
    #[gpui::test]
    fn the_palette_takes_the_larger_navigation_steps(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
        let len = shell.read_with(&cx, |shell, _| {
            shell.palette.as_ref().unwrap().filtered().len()
        });
        assert!(
            len >= 16,
            "sanity: the last assertion below (ctrl+d then ctrl+f, landing \
             at 15) needs at least 16 rows or it fails on ITS OWN clamp \
             instead of proving the step size — a looser bound here would \
             fail at the wrong assertion with a confusing message, got {len}"
        );

        cx.simulate_keystrokes("ctrl-d");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            5,
            "ctrl+d moves down 5"
        );
        cx.simulate_keystrokes("ctrl-f");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            15,
            "ctrl+f moves down 10 more"
        );
        cx.simulate_keystrokes("ctrl-u");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            10,
            "ctrl+u moves back 5"
        );
        cx.simulate_keystrokes("pageup");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            0,
            "pageup is ctrl+b's alias: back 10"
        );
    }

    /// The split this change deliberately preserves (spec §3): the new
    /// larger steps clamp, while the ±1 keys keep wrapping. This test and
    /// its partner above (`the_palette_takes_the_larger_navigation_steps`)
    /// are jointly, not individually, sufficient: that one alone would
    /// pass against a `nav_command` that returned `Move(0)` for every key
    /// (every assertion there stays put or moves by the size actually
    /// under test, never wraps), and this one alone would pass against a
    /// palette that ignored the new keys entirely (every clamp assertion
    /// here is also satisfied by "nothing moved"). Together they pin both
    /// that the new keys move the selection by the right amount AND that
    /// the amount clamps rather than wraps — do not delete one believing
    /// the other still covers navigation.
    #[gpui::test]
    fn palette_big_steps_clamp_while_arrows_still_wrap(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
        let len = shell.read_with(&cx, |shell, _| {
            shell.palette.as_ref().unwrap().filtered().len()
        });

        cx.simulate_keystrokes("ctrl-u");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            0,
            "ctrl+u at the top clamps — a page jump must not teleport to the end"
        );

        cx.simulate_keystrokes("up");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            len - 1,
            "up at the top still wraps to the last result, exactly as before"
        );

        cx.simulate_keystrokes("ctrl-f");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.palette.as_ref().unwrap().selected()),
            len - 1,
            "and ctrl+f at the bottom clamps"
        );
    }

    /// Typing still reaches the query field: the new arm must not swallow
    /// characters on their way to the input.
    #[gpui::test]
    fn the_new_palette_arm_does_not_intercept_typing(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "palette::toggle");
        cx.simulate_input("theme");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell
                .palette
                .as_ref()
                .unwrap()
                .query()
                .to_string()),
            "theme"
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
            roster: crate::module::ModuleRoster::default(),
            restored_tiles: crate::session::TileRecords::new(),
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
            shell.services.workspaces.active().tree().tiles().len()
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

    /// Fix-round regression for the orphaned-`FocusId` finding on
    /// `apply_reload`'s palette-close path (see `pending_focus_restore`'s
    /// and that call site's own doc comments): a background reload closing
    /// the palette while its query `Input` genuinely holds window focus
    /// must still end up with focus back on the shell root — `apply_reload`
    /// itself has no `Window` to do that with directly, so this proves the
    /// `pending_focus_restore` flag actually gets consumed by the very next
    /// render, the same "assert the shell handle is focused after" pattern
    /// `escape_closes_the_palette_without_dispatching` uses for the
    /// ordinary key-driven close.
    #[gpui::test]
    fn apply_reload_closing_a_focused_palette_restores_focus_to_the_shell_root(
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
        let palette_input = shell.read_with(&cx, |shell, _| shell.palette_input.clone());
        let palette_input_focus_handle =
            palette_input.read_with(&cx, |state, cx| state.focus_handle(cx));

        cx.simulate_keystrokes("ctrl-k");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
            "sanity: ctrl+k opening the palette should have focused its query Input"
        );

        // A keymap-differing reload (not just a theme-only one — see the
        // contrasting pair of tests above) closes the palette out from
        // under that still-focused input, with no Window available to
        // `apply_reload` itself to redirect focus.
        let new_config = config_with_mod("ctrl");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));

        assert!(
            shell.read_with(&cx, |shell, _| shell.palette.is_none()),
            "sanity: the keymap-differing reload should have closed the palette"
        );

        // `apply_reload` already calls `cx.notify()` unconditionally, so
        // the next draw is exactly the render that should consume
        // `pending_focus_restore`.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(
            !cx.update(|window, _cx| palette_input_focus_handle.is_focused(window)),
            "the closed palette's query input must not still hold window focus"
        );
        assert!(
            cx.update(|window, _cx| shell_focus_handle.is_focused(window)),
            "a background reload closing a focused palette must still return \
             focus to the shell root — otherwise handle_key_down's on_key_down \
             listener never fires again until a mouse click claims focus"
        );
    }

    // --- Task 6: frame keys, the readout, and config reload -------------

    #[gpui::test]
    fn ctrl_digits_switch_the_frame_slot_and_ctrl_0_clears_it(cx: &mut gpui::TestAppContext) {
        let mut services = test_services();
        // Two slots through config, the way `new` reads them.
        let groupings = LayerDoc::builtin("groupings", "1 = [\"book\"]\n2 = [\"lhu\"]\n").unwrap();
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
        )
        .unwrap();
        services.config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                groupings,
                datasets,
            ],
            ..ConfigSources::default()
        });
        let (window, mut cx) = open_shell(cx, services);
        let shell = shell_of(&window, &mut cx);
        cx.simulate_keystrokes("ctrl-2");
        assert_eq!(
            shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
            Some(2)
        );
        let v = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
        cx.simulate_keystrokes("ctrl-5");
        assert_eq!(
            shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
            Some(2),
            "an empty slot is ignored"
        );
        assert_eq!(shell.read_with(&cx, |s, cx| s.frame.read(cx).versions()), v);
        cx.simulate_keystrokes("ctrl-0");
        assert_eq!(
            shell.read_with(&cx, |s, cx| s.frame.read(cx).active_slot()),
            None
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("frame-readout").is_some(),
            "the readout painted"
        );
    }

    #[gpui::test]
    fn a_reloaded_groupings_doc_replaces_the_slots_and_a_sources_change_asks_for_a_restart(
        cx: &mut gpui::TestAppContext,
    ) {
        let (services, _log) = services_with_recorder();
        let (window, mut cx) = open_shell(cx, services);
        let shell = shell_of(&window, &mut cx);
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&shell, move |_, event: &ShellEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach();
        });

        let mut new_config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                LayerDoc::builtin("groupings", "3 = [\"book\"]\n").unwrap(),
                LayerDoc::builtin("datasets", "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n[risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n").unwrap(),
                LayerDoc::builtin("views", "[v]\ndataset = \"risk\"\ngrouping = [\"book\"]\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        let v0 = shell.read_with(&cx, |s, cx| s.frame.read(cx).versions());
        shell.update(&mut cx, |s, cx| {
            s.apply_reload(std::mem::take(&mut new_config), cx)
        });
        let (slots, versions) = shell.read_with(&cx, |s, cx| {
            (
                s.frame.read(cx).slots().clone(),
                s.frame.read(cx).versions(),
            )
        });
        assert_eq!(slots.label(3).as_deref(), Some("book"));
        assert!(versions.config > v0.config);
        assert!(
            events.borrow().contains(&ShellEvent::ConfigReloaded),
            "{:?}",
            events.borrow()
        );

        // Now a sources change.
        let mut with_sources = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
                LayerDoc::builtin(
                    "sources",
                    "[s]\ndataset = \"risk\"\npaths = [\"/x/*.csv\"]\n",
                )
                .unwrap(),
            ],
            ..ConfigSources::default()
        });
        shell.update(&mut cx, |s, cx| {
            s.apply_reload(std::mem::take(&mut with_sources), cx)
        });
        assert!(
            events
                .borrow()
                .iter()
                .any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("sources"))),
            "{:?}",
            events.borrow()
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("restart-required").is_some(),
            "the status bar says so"
        );
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
        let pending = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
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
                    .map(|(ix, ws)| (ix, ws.tree().layout(Rect::UNIT)))
                    .collect()
            });

        let session::Restored {
            workspaces: mut restored,
            warnings,
            ..
        } = session::load(&session_path);
        assert!(warnings.is_empty(), "{warnings:?}");

        let restored_layout: Vec<(u8, Vec<(crate::tiling::TileId, Rect)>)> = restored
            .spaces()
            .map(|(ix, ws)| (ix, ws.tree().layout(Rect::UNIT)))
            .collect();
        assert_eq!(
            live, restored_layout,
            "restoring the saved session must reproduce every workspace's layout"
        );

        let before_ids: std::collections::HashSet<_> = restored
            .spaces()
            .flat_map(|(_, t)| t.tree().tiles())
            .collect();
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
                .update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx))
                .is_none(),
            "nothing dirty yet — no pending write"
        );

        cx.simulate_keystrokes("ctrl-v");
        let first = shell.update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx));
        assert!(
            first.is_some(),
            "the dispatch above must have marked it dirty"
        );

        assert!(
            shell
                .update(&mut cx, |shell, cx| shell.take_dirty_session_write(cx))
                .is_none(),
            "the dirty flag must be consumed by the first take, not left set"
        );
    }

    /// Fix round 1 (Task 4 review): the headline write trigger —
    /// `take_dirty_session_write`'s `tiles == self.last_tiles_written`
    /// half of its guard, not just `session_dirty` — was untested. A
    /// module state change alone (through the recording module's own
    /// `command`, the same path its `serialize` reads back) never touches
    /// `session_dirty`, so only that tiles comparison can notice it; this
    /// pins that a flush happens exactly once per state change, carrying
    /// the new state, and that a further call with nothing new returns
    /// `None` again.
    #[gpui::test]
    fn a_module_state_change_alone_flushes_once_with_the_new_state(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("session.toml");

        let (mut services, _log) = services_with_recorder();
        services.session_path = Some(session_path);
        let (window, mut vcx) = open_shell(cx, services);
        vcx.simulate_keystrokes("ctrl-v");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut vcx);
        let tile = shell.read_with(&vcx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });

        // Drain the layout-dirty write the split above queued, so what
        // follows isolates the state-only trigger from the
        // already-covered layout one.
        let layout_flush = shell.update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx));
        assert!(
            layout_flush.is_some(),
            "the split must have marked the layout dirty"
        );
        assert!(
            shell
                .update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx))
                .is_none(),
            "nothing changed since that flush — session_dirty is clear and \
             the occupant's state hasn't moved"
        );

        // Mutate the occupant's own state through `command` — never
        // `session_dirty` — exactly the path `serialize` reads back.
        shell.update_in(&mut vcx, |view, window, cx| {
            let o = view.occupants.get(&tile).expect("the split created a tile");
            o.content
                .command("state changed", window, cx)
                .expect("the recorder's command always succeeds");
        });

        let state_flush = shell.update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx));
        let (_, text) = state_flush.expect(
            "a state-only change must still flush — `session_dirty` alone \
             would miss it, which is exactly what this test guards",
        );
        assert!(
            text.contains("last_command = \"state changed\""),
            "the flushed text must carry the new state: {text}"
        );

        assert!(
            shell
                .update(&mut vcx, |shell, cx| shell.take_dirty_session_write(cx))
                .is_none(),
            "the state hasn't changed again since the flush above"
        );
    }

    /// Task 4 (Phase 3 §3.5), two halves of the same contract:
    /// `current_tiles` reports a live occupant's own kind and whatever its
    /// `serialize` returns, and a `restored_tiles` record for a tile that
    /// really is in the restored `Workspaces` reaches that tile's factory
    /// as `Some(state)` when `ensure_occupants` creates it.
    #[gpui::test]
    fn current_tiles_reflects_live_occupants_and_restored_state_reaches_the_factory(
        cx: &mut gpui::TestAppContext,
    ) {
        // Half 1: a freshly created occupant shows up in `current_tiles`
        // under its own kind.
        let (services, _log) = services_with_recorder();
        let (window, mut vcx) = open_shell(cx, services);
        vcx.simulate_keystrokes("ctrl-v");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shell = shell_of(&window, &mut vcx);
        let tile = shell.read_with(&vcx, |s, _| {
            s.services.workspaces.active().focused_tile().unwrap()
        });
        let tiles = shell.read_with(&vcx, |s, cx| s.current_tiles(cx));
        assert_eq!(
            tiles.get(&tile.0).map(|r| r.kind.as_str()),
            Some("rec"),
            "{tiles:?}"
        );

        // Half 2: a hand-built session table restoring one tile (id 1)
        // with a `tiles` record naming the recorder's own kind — built
        // through `session::from_toml`, exactly as `main.rs` restores a
        // real session file — must have its `state` handed to the
        // recorder's `create` as `Some(...)`.
        let mut table = session::to_toml(&Workspaces::new(), &session::TileRecords::new());
        let ws1: toml::Table = r#"
            focused = 1
            [node]
            kind = "leaf"
            id = 1
            [tiles.1]
            module = "rec"
            [tiles.1.state]
            last_command = "hello"
        "#
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws_table)) = table.get_mut("workspaces") {
            ws_table.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = session::from_toml(&table).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
        let expected_state = restored.tiles.get(&1).unwrap().state.clone();

        let (mut services2, log2) = services_with_recorder();
        services2.workspaces = restored.workspaces;
        services2.restored_tiles = restored.tiles;
        let (_window2, mut vcx2) = open_shell(cx, services2);
        vcx2.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            log2.borrow().iter().any(|r| matches!(
                r,
                crate::module::recording::Recorded::Created(TileId(1), Some(state))
                    if *state == expected_state
            )),
            "{:?}",
            log2.borrow()
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
            shell.services.workspaces.active().tree().tiles().len()
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
    /// Every shell dialog's panel starts at the same top edge —
    /// `dialog::MODAL_TOP_RATIO` of the viewport below the backdrop's own
    /// top — rather than being vertically centered (user direction:
    /// differently-sized dialogs centering to different heights defeats
    /// spatial memory; a shared top edge is what the eye expects). Proven
    /// across two differently-sized dialogs: settings (tall) and keyboard
    /// shortcuts must paint their panels at the SAME y, at exactly the
    /// ratio offset.
    #[gpui::test]
    fn all_shell_dialogs_share_the_same_top_edge(cx: &mut gpui::TestAppContext) {
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

        let open_and_measure = |cx: &mut gpui::VisualTestContext, action: &str| {
            cx.update(|window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.dispatch(&ActionId(action.to_string()), None, window, cx);
                });
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let backdrop = cx
                .debug_bounds("shell-modal-backdrop")
                .expect("backdrop painted");
            let panel = cx.debug_bounds("shell-modal-panel").expect("panel painted");
            // Close again (escape path) so the next dialog can open.
            cx.simulate_keystrokes("escape");
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            (
                f32::from(panel.origin.y) - f32::from(backdrop.origin.y),
                f32::from(backdrop.size.height),
                f32::from(backdrop.origin.y),
            )
        };

        let (settings_top, backdrop_height, backdrop_origin_y) =
            open_and_measure(&mut cx, "settings::open");
        let (keybindings_top, _, _) = open_and_measure(&mut cx, "keybindings::open");

        let expected = backdrop_height * dialog::MODAL_TOP_RATIO;
        assert!(
            (settings_top - expected).abs() < 1.0,
            "the settings panel should start MODAL_TOP_RATIO down the \
             backdrop, expected {expected}, got {settings_top}"
        );
        assert_eq!(
            settings_top, keybindings_top,
            "differently-sized dialogs must share the same top edge, got \
             {settings_top} vs {keybindings_top}"
        );

        // The command palette shares the line too (user direction: it's
        // dialog-like). It renders in the same coordinate space the modal
        // backdrop does (both absolute children of ShellView's root), so
        // the captured backdrop origin is the shared reference point.
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("palette::toggle".to_string()), None, window, cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let palette_panel = cx
            .debug_bounds("palette-panel")
            .expect("palette panel painted");
        let palette_top = f32::from(palette_panel.origin.y) - backdrop_origin_y;
        assert!(
            (palette_top - expected).abs() < 1.0,
            "the palette panel should start on the same shared top edge, \
             expected {expected}, got {palette_top}"
        );
    }

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
                shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
            });
        });

        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "settings::open should have set ShellView's own modal state"
        );

        // The workspace itself must stay untouched — settings::open is not
        // a workspace verb and must not be mistaken for one.
        let tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
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
            shell.services.workspaces.active().tree().tiles().len()
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

    #[gpui::test]
    fn opening_the_settings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        assert!(shell.read_with(&cx, |shell, _| shell.settings.is_some()));
        assert!(
            filter_is_focused(&shell, &mut cx),
            "the filter must own focus the moment the dialog opens"
        );
    }

    /// Typing filters; the old `h`/`l` stepping keys are now just text,
    /// and must not step anything on their way into the query.
    #[gpui::test]
    fn typing_filters_the_settings_rows(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
        cx.simulate_input("dark");
        let (query, selected) = shell.read_with(&cx, |shell, _| {
            let state = shell.settings.as_ref().unwrap();
            (state.query.clone(), state.selected)
        });
        assert_eq!(query, "dark");
        assert_eq!(selected, 0, "a query selects the top match");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark()),
            before,
            "typing must never apply a setting — the old h/l/enter stepping \
             keys are plain text now"
        );
    }

    /// tab steps the selected row's value forward and shift+tab back,
    /// through the same apply path a click takes. Narrowed to the Font
    /// size row rather than the (two-value) Dark mode row deliberately:
    /// on a two-value row `step(2, current, Left)` and
    /// `step(2, current, Right)` land on the same value, so asserting
    /// only "the value changed" either direction can't tell a correct
    /// `StepDirection::Left`/`Right` mapping in `handle_key` from an
    /// accidentally swapped one. Font size has three values
    /// (Small/Medium/Large), so asserting the EXACT target after each
    /// key -- not just "it changed" -- genuinely pins the direction: a
    /// swapped mapping would land tab on Small, not Large.
    #[gpui::test]
    fn tab_and_shift_tab_step_the_selected_value(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_input("font");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.font_size),
            crate::fontsize::FontSize::Medium,
            "sanity: the test shell starts at the Medium default"
        );

        cx.simulate_keystrokes("tab");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.font_size),
            crate::fontsize::FontSize::Large,
            "tab should step Font size forward, Medium -> Large"
        );

        cx.simulate_keystrokes("shift-tab");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.font_size),
            crate::fontsize::FontSize::Medium,
            "shift+tab should step it back, Large -> Medium"
        );
    }

    /// The three `apply_setting` arms `tab_and_shift_tab_step_the_
    /// selected_value` doesn't reach (that test covers Font size) --
    /// Theme, Dark mode, Find style -- each stepped once through a real
    /// `tab` keystroke, so a mis-wired arm (e.g. `SettingId::Theme =>
    /// set_font_size_on`) is caught here rather than nowhere: `
    /// apply_setting` takes `&mut ShellView` and has no pure unit test of
    /// its own. One fresh dialog per row rather than one dialog walked
    /// with `j`/`k` (as the retired vim-nav version of this test did):
    /// selecting a different row now means typing a different filter
    /// query, and there's no key that clears the shared field back to
    /// empty mid-session, so three small dialogs are simpler than one
    /// that fights its own filter.
    #[gpui::test]
    fn tab_steps_every_remaining_apply_setting_arm(cx: &mut gpui::TestAppContext) {
        {
            let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
            let before = shell.read_with(&cx, |shell, _| {
                shell.services.theme.active_name().to_string()
            });
            cx.simulate_input("theme");
            cx.simulate_keystrokes("tab");
            let after = shell.read_with(&cx, |shell, _| {
                shell.services.theme.active_name().to_string()
            });
            assert_ne!(
                after, before,
                "tab on the Theme row should step to a different theme"
            );
        }

        {
            let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
            let before =
                shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
            cx.simulate_input("dark");
            cx.simulate_keystrokes("tab");
            let after =
                shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());
            assert_eq!(after, !before, "tab on the Dark mode row should toggle it");
        }

        {
            let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
            let before = shell.read_with(&cx, |shell, _| shell.find_style);
            cx.simulate_input("keyboard");
            cx.simulate_keystrokes("tab");
            let after = shell.read_with(&cx, |shell, _| shell.find_style);
            assert_ne!(
                after, before,
                "tab on the Find style row should flip vim/fzf"
            );
        }
    }

    /// Enter is inert and reserved here (spec §3): it must not step a
    /// value, and must not close the dialog either.
    #[gpui::test]
    fn enter_does_nothing_in_the_settings_dialog(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_input("dark");
        let before = shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

        cx.simulate_keystrokes("enter");
        shell.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.services.theme.active_mode().is_dark(),
                before,
                "enter must not step the value"
            );
            assert!(shell.modal.is_some(), "and must not close the dialog");
        });
    }

    /// The full inertness contract for the reserved `enter` (spec §3):
    /// with a focused `Input`, `handle_key` returning `false` for it would
    /// NOT make it inert — `enter` would reach the filter, be normalized
    /// away to an empty edit, but still fire an unconditional
    /// `InputEvent::Change` that resets `selected` back to the top match
    /// via `SettingsState::set_query`. `handle_key` claims it instead (see
    /// its own doc comment). Unlike `enter_does_nothing_in_the_settings_
    /// dialog` above, this moves the selection off the top row FIRST, so
    /// a reset back to 0 is actually observable.
    #[gpui::test]
    fn enter_is_reserved_and_leaves_the_settings_dialog_untouched(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_keystrokes("down down");
        let selected_before =
            shell.read_with(&cx, |shell, _| shell.settings.as_ref().unwrap().selected);
        assert_eq!(
            selected_before, 2,
            "sanity: two downs land on the third row"
        );
        let dark_before =
            shell.read_with(&cx, |shell, _| shell.services.theme.active_mode().is_dark());

        cx.simulate_keystrokes("enter");

        let (query, selected, dark_after, open) = shell.read_with(&cx, |shell, _| {
            let state = shell.settings.as_ref().unwrap();
            (
                state.query.clone(),
                state.selected,
                shell.services.theme.active_mode().is_dark(),
                shell.modal.is_some(),
            )
        });
        assert!(
            query.is_empty(),
            "enter must not leave any character in the filter"
        );
        assert_eq!(
            selected, selected_before,
            "enter must not reset the selection to the top match"
        );
        assert_eq!(dark_after, dark_before, "enter must not step any value");
        assert!(open, "enter must not close the dialog");
    }

    #[gpui::test]
    fn escape_closes_the_settings_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "settings::open");
        cx.simulate_keystrokes("escape");
        shell.read_with(&cx, |shell, _| {
            assert!(shell.modal.is_none());
            assert!(shell.settings.is_none(), "close_modal clears dialog state");
        });
        assert!(
            cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
            "focus lands back on the shell root"
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

    /// Content-collapse guard, retargeted at the home-rolled dialog:
    /// `settings_open_opens_the_modal` only proves the backdrop/panel/
    /// title chrome painted with non-zero bounds — all painted directly by
    /// `dialog::render_modal` itself, so it would stay green even if the
    /// row list nested inside the panel rendered at zero height (the exact
    /// failure mode the old gpui-component `Settings` composite hit when
    /// its percentage-height root met a parent with no definite height).
    /// This test checks the thing that test doesn't: the row list
    /// (`debug_selector("settings-list")`) must paint at its full derived
    /// height — `settings_view` has four rows (Theme, Dark mode, Font
    /// size, Find style), each `ROW_HEIGHT` (44px) tall — and every one of
    /// those rows must itself have painted bounds.
    ///
    /// Uses `WindowOptions::default()`, same as every other modal test in
    /// this file — gpui's own `default_bounds` gives that a realistic
    /// 1536x1095 test window, not a cramped one, so a collapse here is not
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

        // settings::open is bound to ctrl+, (rebound from mod+, in commit
        // 87aa731; this test merged in concurrently and carried the old key).
        cx.simulate_keystrokes("ctrl-,");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            shell.read_with(&cx, |shell, _| shell.modal.is_some()),
            "sanity: ctrl-, should have opened the settings modal"
        );

        let list_bounds = cx
            .debug_bounds("settings-list")
            .expect("the settings row list should have painted bounds");
        assert!(
            list_bounds.size.height >= px(4.0 * 44.0),
            "the settings row list should paint at its full four-row height \
             (4 × ROW_HEIGHT = 176px) — got {:?}. A sliver here means the \
             list collapsed inside the modal panel instead of laying out \
             its rows.",
            list_bounds.size
        );
        // debug_bounds takes &'static str, so the four selectors are spelled
        // out rather than formatted.
        for selector in [
            "settings-row-0",
            "settings-row-1",
            "settings-row-2",
            "settings-row-3",
        ] {
            assert!(
                cx.debug_bounds(selector).is_some(),
                "{selector} should have painted bounds (Theme, Dark mode, \
                 Font size, Find style rows must all lay out)"
            );
        }
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
            px(12.0),
            "sanity: with no [ui] font_size configured, medium (12px — well \
             below gpui's own 16px rem default, per the fontsize \
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
            px(14.0),
            "the render after set_font_size(Large) should apply 14px as the \
             window rem size"
        );
    }

    /// End-to-end: `ctrl+=` / `ctrl+-` (`fontsize::increase`/`decrease`)
    /// step the UI font size through real keystrokes, clamped at both ends
    /// — the keyboard path onto the same state the settings toggle group
    /// drives.
    #[gpui::test]
    fn ctrl_equals_and_minus_step_the_font_size_with_clamping(cx: &mut gpui::TestAppContext) {
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
        let font_size =
            |cx: &gpui::VisualTestContext| shell.read_with(cx, |shell, _| shell.font_size);

        assert_eq!(font_size(&cx), crate::fontsize::FontSize::Medium);

        cx.simulate_keystrokes("ctrl-=");
        assert_eq!(font_size(&cx), crate::fontsize::FontSize::Large);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            cx.update(|window, _cx| window.rem_size()),
            px(14.0),
            "ctrl+= should have applied Large's 14px rem size"
        );

        cx.simulate_keystrokes("ctrl-=");
        assert_eq!(
            font_size(&cx),
            crate::fontsize::FontSize::Large,
            "increase clamps at Large"
        );

        cx.simulate_keystrokes("ctrl--");
        cx.simulate_keystrokes("ctrl--");
        assert_eq!(font_size(&cx), crate::fontsize::FontSize::Small);
        cx.simulate_keystrokes("ctrl--");
        assert_eq!(
            font_size(&cx),
            crate::fontsize::FontSize::Small,
            "decrease clamps at Small"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            cx.update(|window, _cx| window.rem_size()),
            px(10.0),
            "two decreases from Large should land on Small's 10px rem size"
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
            px(10.0),
            "[ui] font_size = \"small\" should render at a 10px rem size \
             from the very first frame"
        );
    }

    /// `settings_view::set_find_style` (the find-style button group's
    /// setter, driven directly for the same reason `set_theme`'s and
    /// `set_font_size`'s tests drive the handler rather than the control)
    /// updates `ShellView::find_style` — the state `keybindings_view`
    /// reads fresh on every keystroke and render, so there is nothing
    /// further to apply.
    #[gpui::test]
    fn set_find_style_updates_the_shell_state(cx: &mut gpui::TestAppContext) {
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
            shell.read_with(&cx, |shell, _| shell.find_style),
            FindStyle::Vim,
            "sanity: with no [ui] find_style configured, vim is the default"
        );

        cx.update(|_window, cx| settings_view::set_find_style(&shell, FindStyle::Fzf, cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.find_style),
            FindStyle::Fzf,
            "set_find_style should update the shell's state immediately"
        );
    }

    /// `[ui] find_style` resolves at startup and re-resolves on hot reload
    /// — the same two paths `font_size` rides (`ShellView::new` /
    /// `apply_reload`).
    #[gpui::test]
    fn a_configured_find_style_resolves_at_startup_and_on_reload(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);

        let mut services = test_services();
        services.config = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfind_style = \"fzf\"\n").unwrap()],
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

        let root = window.root(&mut cx).unwrap();
        let shell = root.read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });

        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.find_style),
            FindStyle::Fzf,
            "[ui] find_style = \"fzf\" should resolve at startup"
        );

        // A reload whose config lacks the key falls back to vim — the
        // same lenient re-derive `font_size` gets in `apply_reload`.
        let new_config = config_with_mod("alt");
        shell.update(&mut cx, |shell, cx| shell.apply_reload(new_config, cx));
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.find_style),
            FindStyle::Vim,
            "a reload without [ui] find_style should re-resolve to the default"
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

        // Open the settings modal through the real dispatch path —
        // `settings_view::open` routes through `open_shell_dialog_with_key`
        // (the one standard dialog door, keyed since the row-list rewrite).
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
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
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(tile_count, 3, "should have created three tiles");

        // Record the tile ids in tree order before focusing the middle one.
        let tiles_before = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles()
        });
        assert_eq!(tiles_before.len(), 3);

        // After three splits, the focused tile is the last one (tiles_before[2]).
        // Focus the middle tile (at index 1) using focus_left (mod+h).
        cx.simulate_keystrokes("alt-h");

        let focused_tile = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });
        assert_eq!(
            focused_tile,
            Some(tiles_before[1]),
            "should have focused the middle tile (one position left)"
        );

        // Close the middle tile (ctrl+w).
        cx.simulate_keystrokes("ctrl-w");

        // Verify we have two tiles left.
        let remaining_tile_count = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles().len()
        });
        assert_eq!(remaining_tile_count, 2, "should have two tiles after close");

        let tiles_after = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().tiles()
        });
        // tiles_after should be [tiles_before[0], tiles_before[2]]
        assert_eq!(tiles_after, vec![tiles_before[0], tiles_before[2]]);

        // Assert that the focused tile is tiles_before[2] (the adjacent sibling in tree order).
        let focused_after_close = shell.read_with(&cx, |shell, _| {
            shell.services.workspaces.active().tree().focused()
        });

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
                shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
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

    /// Open a real window with a real `ShellView`, draw a frame, dispatch
    /// `action`, draw again — the preamble every dialog test here needs.
    fn dialog_test_shell(
        cx: &mut gpui::TestAppContext,
        action: &str,
    ) -> (Entity<ShellView>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        // Same reclaimed keybindings `main` registers in production
        // (`dialog::init_reclaimed_keybindings`'s own doc comment has the
        // full mechanism for each) — without this, a dialog test that
        // presses tab would prove nothing: gpui-component's `Root` would
        // still silently consume it exactly as it does in an unpatched
        // window.
        cx.update(dialog::init_reclaimed_keybindings);
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(test_services(), None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let root = window.root(&mut vcx).unwrap();
        let shell = root.read_with(&vcx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        vcx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId(action.to_string()), None, window, cx);
            });
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (shell, vcx)
    }

    /// Does the shared dialog filter currently hold focus?
    fn filter_is_focused(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| {
            shell
                .read(cx)
                .dialog_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        })
    }

    /// Opening the dialog focuses the shared filter, so the first
    /// character typed filters instead of falling on the floor.
    #[gpui::test]
    fn opening_the_keybindings_dialog_focuses_the_filter(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        assert!(
            shell.read_with(&cx, |shell, _| shell.keybindings.is_some()),
            "sanity: keybindings::open should have opened the dialog"
        );
        assert!(
            filter_is_focused(&shell, &mut cx),
            "the filter must own focus the moment the dialog opens"
        );
    }

    /// The retired vim motion is now plain text: `j` types a `j` and
    /// leaves the selection where it was.
    #[gpui::test]
    fn typing_j_filters_rather_than_moving_the_selection(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_input("j");
        let (query, selected) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.query.clone(), state.selected)
        });
        assert_eq!(query, "j", "j must reach the filter as text");
        assert_eq!(selected, 0, "j must not move the selection any more");
    }

    /// Arrow and ctrl motions still move the selection, and do it without
    /// disturbing the filter's focus or its text.
    #[gpui::test]
    fn arrows_and_ctrl_motions_move_the_selection(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("down down");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
            2,
            "two downs should land on the third row"
        );
        cx.simulate_keystrokes("ctrl-u");
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected),
            0,
            "ctrl+u steps back 5, clamped at the top of the list"
        );
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .query
                .is_empty()),
            "navigation must not put anything in the filter"
        );
        assert!(
            filter_is_focused(&shell, &mut cx),
            "navigation must not steal focus from the filter"
        );
    }

    /// `tab` is reserved here (it steps values in the settings dialog,
    /// which has nothing to step) and must be genuinely inert: with a
    /// focused `Input`, `handle_key` returning `false` for it would NOT
    /// make it inert — the key would continue to the filter's own
    /// text-input phase, and `InputState::normalize_input` strips only
    /// `\n`/`\r`, not `\t`, so it would land as a literal tab character
    /// and collapse the list to "no matches". `handle_key` claims it
    /// instead (see its own doc comment).
    #[gpui::test]
    fn tab_is_reserved_and_leaves_the_keybindings_dialog_untouched(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("down down");
        let selected_before =
            shell.read_with(&cx, |shell, _| shell.keybindings.as_ref().unwrap().selected);
        assert_eq!(
            selected_before, 2,
            "sanity: two downs land on the third row"
        );

        cx.simulate_keystrokes("tab");

        let (query, selected) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.query.clone(), state.selected)
        });
        assert!(
            query.is_empty(),
            "tab must not leak a literal tab character into the filter"
        );
        assert_eq!(selected, selected_before, "tab must not move the selection");
    }

    /// Enter blurs the filter so rebind capture sees raw keys: the letter
    /// lands in the pending binding, NOT in the query.
    #[gpui::test]
    fn enter_starts_listening_and_a_letter_is_captured_not_typed(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("enter");
        assert!(
            shell.read_with(&cx, |shell, _| shell
                .keybindings
                .as_ref()
                .unwrap()
                .listening
                .is_some()),
            "enter should start listening on the selected row"
        );
        assert!(
            !filter_is_focused(&shell, &mut cx),
            "listening must blur the filter, or the capture can never see a letter"
        );

        cx.simulate_input("j");
        let (pending, query) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (state.listening.clone(), state.query.clone())
        });
        assert_eq!(
            pending.as_deref().map(<[_]>::len),
            Some(1),
            "the letter must be captured as the new binding"
        );
        assert!(
            query.is_empty(),
            "and must NOT have been typed into the filter"
        );
    }

    /// Escape cancels the capture, refocuses the filter, and leaves both
    /// the query and the dialog itself alone.
    #[gpui::test]
    fn escape_cancels_a_capture_without_closing_the_dialog(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_input("f");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("j");
        cx.simulate_keystrokes("escape");

        let (listening, query, open) = shell.read_with(&cx, |shell, _| {
            let state = shell.keybindings.as_ref().unwrap();
            (
                state.listening.is_some(),
                state.query.clone(),
                shell.modal.is_some(),
            )
        });
        assert!(!listening, "escape should cancel the capture");
        assert!(open, "and must not also close the dialog behind it");
        assert_eq!(query, "f", "the filter text survives a cancelled capture");
        assert!(
            filter_is_focused(&shell, &mut cx),
            "cancelling hands focus back to the filter"
        );
    }

    /// Escape from the resting state closes the dialog and returns focus
    /// to the shell root, so shell chords work again immediately.
    #[gpui::test]
    fn escape_closes_the_dialog_and_restores_shell_focus(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_keystrokes("escape");
        shell.read_with(&cx, |shell, _| {
            assert!(shell.modal.is_none(), "escape should close the modal");
            assert!(
                shell.keybindings.is_none(),
                "close_modal must clear the dialog state too, or the shared \
                 input's subscription can route into a stale dialog"
            );
        });
        assert!(
            !filter_is_focused(&shell, &mut cx),
            "focus must leave the filter on close"
        );
        assert!(
            cx.update(|window, cx| shell.read(cx).focus_handle.is_focused(window)),
            "and land back on the shell root"
        );
    }

    /// The filter narrows what actually *paints*, and the narrowed list
    /// renders its fuzzy highlights without blowing up — the painted half
    /// of this dialog's conversion, re-expressing what the retired fzf
    /// find test used to prove about its own narrowed list. Row selectors
    /// stay keyed by full-list index, so a surviving row and a hidden one
    /// can be addressed by identity.
    #[gpui::test]
    fn typing_a_query_narrows_the_rows_that_paint(cx: &mut gpui::TestAppContext) {
        let (shell, mut cx) = dialog_test_shell(cx, "keybindings::open");
        cx.simulate_input("focus");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (visible, row_count) = shell.read_with(&cx, |shell, _| {
            let rows =
                keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap);
            let state = shell.keybindings.as_ref().expect("dialog open");
            let visible: Vec<usize> = keybindings_view::visible_rows(state, &rows)
                .iter()
                .map(|m| m.row)
                .collect();
            (visible, rows.len())
        });
        assert!(
            !visible.is_empty() && visible.len() < row_count,
            "sanity: 'focus' should match some rows but not all, got {visible:?} of {row_count}"
        );

        let first_hidden = (0..row_count).find(|ix| !visible.contains(ix)).unwrap();
        // `debug_bounds` takes `&'static str`; leak the two dynamic
        // selectors (test-only, a few bytes).
        let match_selector: &'static str =
            Box::leak(format!("keybindings-row-{}", visible[0]).into_boxed_str());
        let hidden_selector: &'static str =
            Box::leak(format!("keybindings-row-{first_hidden}").into_boxed_str());
        assert!(
            cx.debug_bounds(match_selector).is_some(),
            "a matching row must still paint under the filter"
        );
        assert!(
            cx.debug_bounds(hidden_selector).is_none(),
            "a non-matching row (index {first_hidden}) must not paint under the filter"
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
                shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
            });
        });

        // The action bound at row 0 (top of sort order) at the moment the
        // dialog opened — what the capture below should end up bound to.
        let target_action = shell.read_with(&cx, |shell, _| {
            keybindings_view::derive_rows(&shell.services.registry, &shell.services.keymap)[0]
                .action
                .clone()
        });

        cx.simulate_keystrokes("enter");
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
                shell.dispatch(&ActionId("keybindings::open".to_string()), None, window, cx);
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

    /// Spec §7.4's debug overlay toggle, end to end through the real key
    /// pipeline: `mod+shift+p` (alt is the test/default mod) dispatches
    /// `perf::toggle_overlay`, which paints the readout panel; a second
    /// press removes it. Bounds via the `perf-overlay` debug selector —
    /// the same honest what-the-test-can-see contract as
    /// `empty_workspace_paints_the_hint`.
    #[gpui::test]
    fn perf_overlay_toggles_via_the_bound_action(cx: &mut gpui::TestAppContext) {
        let (window, mut cx) = open_shell(cx, test_services());
        let shell = shell_of(&window, &mut cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("perf-overlay").is_none(),
            "the overlay must start hidden"
        );

        cx.simulate_keystrokes("alt-shift-p");
        assert!(
            shell.read_with(&cx, |shell, _| shell.perf_overlay),
            "alt+shift+p should dispatch perf::toggle_overlay and set the flag"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx.debug_bounds("perf-overlay");
        assert!(
            bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the perf overlay should paint with non-zero bounds, got {bounds:?}"
        );

        cx.simulate_keystrokes("alt-shift-p");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("perf-overlay").is_none(),
            "a second toggle should remove the overlay"
        );
    }

    /// Spec §7's vertical slice, end to end through the real key pipeline:
    /// `mod+shift+d` dispatches `data::toggle_probe`, a snapshot pushed in
    /// by the binary paints, and the attribution rules are visible in what
    /// painted. This is the only place the §7.1 budget's *painted frame*
    /// half is exercised at all — the benchmarks stop at the snapshot.
    #[gpui::test]
    fn the_data_probe_paints_a_pushed_snapshot(cx: &mut gpui::TestAppContext) {
        use geode_core::attribution::{Attribution, ScopeSemantics};
        use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

        let (window, mut cx) = open_shell(cx, test_services());
        let shell = shell_of(&window, &mut cx);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("data-probe").is_none(),
            "the probe must start hidden"
        );

        // The §6.3 example: an additive measure and a coarse one, the
        // latter non-attributable at the depth below its own grain.
        let meta = |name: &str, by_depth: Vec<Attribution>| ColumnMeta {
            name: name.into(),
            attribution_by_depth: by_depth,
            scope_semantics: ScopeSemantics::Direct,
        };
        let snapshot = Snapshot::for_tests(
            vec![
                (
                    meta("lhu", vec![Attribution::Additive; 2]),
                    TestColumn::Str(vec![None, Some("LHU1")]),
                ),
                (
                    meta("row_depth", vec![Attribution::Additive; 2]),
                    TestColumn::I64(vec![0, 1]),
                ),
                (
                    meta("delta01", vec![Attribution::Additive; 2]),
                    TestColumn::F64(vec![Some(30.0), Some(30.0)]),
                ),
                (
                    meta(
                        "daily_trading_pnl",
                        vec![Attribution::Additive, Attribution::NonAttributable],
                    ),
                    TestColumn::F64(vec![Some(7.0), Some(7.0)]),
                ),
            ],
            1,
        );
        shell.update(&mut cx, |shell, cx| {
            shell.set_probe(
                crate::dataprobe::ProbeState {
                    snapshot: Some(std::sync::Arc::new(snapshot)),
                    freshness: vec![("BK000".into(), "2026-08-30T14:32:00Z".into(), 47)],
                    query_micros: 22_700,
                    error: None,
                },
                cx,
            );
        });

        cx.simulate_keystrokes("alt-shift-d");
        assert!(
            shell.read_with(&cx, |shell, _| shell.data_probe_visible()),
            "alt+shift+d should dispatch data::toggle_probe"
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx.debug_bounds("data-probe");
        assert!(
            bounds.is_some_and(|b| b.size.width > px(0.0) && b.size.height > px(0.0)),
            "the probe should paint with non-zero bounds, got {bounds:?}"
        );

        cx.simulate_keystrokes("alt-shift-d");
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("data-probe").is_none(),
            "a second toggle should remove the probe"
        );
    }

    /// The recording seam: every `ShellView::render` after the first
    /// records one frame-interval sample (consecutive test draws are far
    /// below `perf::IDLE_CUTOFF`), and `perf::reset` zeroes the counters
    /// through the same dispatch chain every other action uses. Recording
    /// itself must not notify — pinned here by the count being exactly
    /// the number of draws driven, with no runaway extra frames.
    #[gpui::test]
    fn render_records_frame_samples_and_reset_clears_them(cx: &mut gpui::TestAppContext) {
        let (window, mut cx) = open_shell(cx, test_services());
        let shell = shell_of(&window, &mut cx);
        // Window-open itself already drew at least once (the very first
        // render records nothing — no previous render to measure from —
        // but any second one records), so take the count after an
        // explicit draw as the baseline rather than assuming 0.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let baseline = shell.read_with(&cx, |shell, _| shell.perf.count());

        // Dirty the view and draw: each re-render past the first records
        // a sample. (A notify can flush into its own automatic test draw
        // in addition to the explicit one, so this asserts growth per
        // round, not an exact per-draw delta.)
        let mut last = baseline;
        for _ in 0..3 {
            shell.update(&mut cx, |_, cx| cx.notify());
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let count = shell.read_with(&cx, |shell, _| shell.perf.count());
            assert!(
                count > last,
                "a dirtied re-render should record at least one sample \
                 (was {last}, now {count})"
            );
            last = count;
        }
        assert!(
            shell.read_with(&cx, |shell, _| shell.perf.max_micros()) > 0,
            "recorded samples should carry a real nonzero interval"
        );

        // Recording must not itself notify (it would turn the shell into a
        // permanent redraw loop): once effects settle, the count stays put.
        cx.run_until_parked();
        let settled = shell.read_with(&cx, |shell, _| shell.perf.count());
        cx.run_until_parked();
        assert_eq!(
            shell.read_with(&cx, |shell, _| shell.perf.count()),
            settled,
            "no further samples may appear without a real invalidation"
        );

        // `perf::reset` zeroes the counters through the same dispatch
        // chain every action uses. Asserted inside the update, before the
        // notify it issues flushes into a fresh (recorded) repaint.
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.dispatch(&ActionId("perf::reset".to_string()), None, window, cx);
                assert_eq!(
                    shell.perf.count(),
                    0,
                    "perf::reset should zero the histogram"
                );
            });
        });
    }
}
