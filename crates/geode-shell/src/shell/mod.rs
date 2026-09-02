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
pub mod perf_overlay;
#[cfg(feature = "profiling")]
pub mod profiling_hook;
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
    App, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollHandle, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Root, TITLE_BAR_HEIGHT, WindowExt as _, h_flex, v_flex};

use crate::actions::{ActionId, ActionRegistry};
use crate::defaults::mod_alias_from_config;
use crate::fonts;
use crate::fontsize::{self, FontSize};
use crate::keymap::{KeyContext, Keymap, MatchResult, Matcher, Modifiers, build_keymap};
use crate::listfilter;
use crate::palette::{self, PaletteItem, PaletteState};
use crate::perf::{self, FrameHistogram};
use crate::reload;
use crate::session;
use crate::theme;
use crate::theme::ThemeService;
use crate::tiling::{
    DIVIDER_HIT_WIDTH, DividerAddress, DockSide, DropTarget, DropZone, Orientation, Rect, TileId,
    Workspaces, apply_workspace_action, divider_strips, dock_edge_strips, drop_highlight_rect,
    locate_drop_target,
};
use crate::vimfind::{self, FindStyle};
use geode_core::config::{Config, LayerDoc};

/// How often the background reload watcher polls the watched config
/// directories' `*.toml` mtimes (brief: "~500ms"). File scanning and
/// `Config::load` themselves run off the UI thread (`cx.background_executor
/// ().spawn`); only the cheap decision + entity mutation happens on the UI
/// thread, via the async entity handle (spec PHILOSOPHY.md: "nothing may
/// stall the render thread").
const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// gpui hover-group name shared by every divider strip (drag-splitters
/// task): the strip is the group, its inner 2px line is the member that
/// tints on `group_hover`. One shared name is correct — gpui resolves a
/// `group_hover` against the *innermost enclosing* group's bounds during
/// paint, so each strip's line only lights for its own strip (precedent:
/// gpui-component's `ResizeHandle` shares the name "handle" across every
/// handle the same way).
const DIVIDER_GROUP: &str = "divider-strip";

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

/// What an in-flight divider drag is resizing (drag-splitters task): a
/// divider inside the main tree, a divider inside one dock's tree, or a
/// dock's frame edge. Tree dividers are named by the pure, stable
/// [`DividerAddress`] captured at mouse-down — never a reference into the
/// tree, because the tree can change between the mouse-down and the moves
/// that apply the drag (`Tree::drag_divider` no-ops on a stale address).
#[derive(Debug, Clone, PartialEq)]
enum DividerDragTarget {
    MainTree {
        address: DividerAddress,
    },
    DockTree {
        side: DockSide,
        address: DividerAddress,
    },
    DockEdge {
        side: DockSide,
    },
}

/// An active divider drag, recorded by the strip's mouse-down and consumed
/// by the full-window drag catcher's mouse-move/up (see `render`). Holds
/// everything the position→ratio math needs so a mouse-move never has to
/// re-derive layout outside the render pass's single geometry walk:
/// `bounds` is the containing rect in *window* coordinates (the laid-out
/// tree/dock rect for tree dividers, the whole content area for dock
/// edges — mouse events arrive in window space, so the rect is stored
/// pre-offset by the sidebar/toolbar chrome rather than converting every
/// event), `axis` picks the resize cursor while dragging, `epoch` is
/// [`Workspaces::switch_epoch`] at mouse-down (review fix: the keyboard
/// stays live during a drag, so mod+N can switch workspaces mid-drag —
/// without this pin the next move would walk the NEW workspace's tree
/// with the OLD one's address and bounds, and a structurally-valid
/// address would apply to the wrong tree; a mismatch cancels the drag
/// instead. Post-merge review finding 7 upgraded the pin from the
/// workspace INDEX to the switch epoch: index equality let a
/// switch-away-and-back with no render between look like "never left" —
/// the one-frame ABA — while the epoch bumps on every actual switch, so
/// it can only compare equal when no switch happened at all), and
/// `moved` records whether any move actually changed the
/// layout. `moved` latches on the first actual change and stays latched:
/// a drag that wanders and returns to its exact starting position still
/// dirties the session on release — accepted, recorded honestly, since
/// the write is a cheap coalesced no-op and un-latching would need
/// per-drag snapshots for a case nobody will notice.
#[derive(Debug, Clone, PartialEq)]
struct DividerDrag {
    target: DividerDragTarget,
    bounds: Rect,
    axis: Orientation,
    epoch: u64,
    moved: bool,
}

/// One paintable divider strip for the current frame, produced inside
/// `render`'s single layout pass: the hit rect in surface coordinates
/// (the strips are absolutely-positioned children of the tile surface),
/// plus the ready-made [`DividerDrag`] ingredients its mouse-down
/// captures.
struct StripSpec {
    rect: Rect,
    axis: Orientation,
    target: DividerDragTarget,
    drag_bounds: Rect,
}

/// Movement (in px) a mod+mouse-down must travel before it becomes a real
/// tile drag, measured per-axis (Chebyshev — `max(|dx|, |dy|)`, the
/// cheapest metric and indistinguishable from Euclidean at this radius).
/// Recorded choice from the approved design's 4–6px range: 5px, the
/// middle — small enough that a deliberate drag never feels gated, large
/// enough that the hand tremor of a sloppy mod+click can never rearrange
/// the layout.
const TILE_DRAG_THRESHOLD: f32 = 5.0;

/// The drag ghost's fixed outline size and its offset from the cursor
/// (recorded choice: a small fixed-size 96×64 outline rect — theme
/// `primary` border, no fill — NOT a copy of the tile content and not
/// scaled to the tile: the ghost only needs to say "a tile is in hand",
/// and a fixed size keeps it legible whether the grabbed tile was a
/// full-height column or a thin dock sliver). Offset down-right so the
/// cursor tip — the thing doing the zone targeting — stays unobscured.
const TILE_DRAG_GHOST_SIZE: (f32, f32) = (96.0, 64.0);
const TILE_DRAG_GHOST_OFFSET: f32 = 12.0;

/// An in-flight mod+drag of a tile (tile-drag task), recorded by a tile
/// body's mod+mouse-down and consumed by its own full-window drag catcher
/// (see `render` — the same capture mechanism as [`DividerDrag`]'s
/// catcher). Nothing is applied until the drop: the drag holds only the
/// grabbed tile's id, the workspace switch epoch at mouse-down (pinned
/// for the same switch-mid-drag reason as `DividerDrag::epoch`, ABA-
/// proofing included — see that field's doc), and
/// cursor positions in window space. That makes cancel truly free —
/// clearing this state undoes nothing and dirties nothing, unlike a
/// divider drag whose cancel must preserve already-applied resizes.
///
/// `active` is the movement threshold latch: false from mouse-down until
/// the cursor travels [`TILE_DRAG_THRESHOLD`] px from `origin`, so a
/// sloppy mod+click can never rearrange the layout. Recorded decisions:
/// - mod+down does NOT change focus at arm time — focus follows the moved
///   tile only on a successful drop, and an abandoned below-threshold
///   mod+click leaves everything untouched, focus included.
/// - The mod key does NOT need to stay held once the drag is armed
///   (standard WM behavior — releasing the modifier mid-drag continues
///   the drag; only the mouse button's release ends it). Modifier state
///   is read once, from the `MouseDownEvent`'s own `modifiers` field —
///   verified against the pinned platform sources: macOS fills it from
///   the native event's `modifierFlags` (`gpui_macos/src/events.rs`,
///   `read_modifiers` — `NSAlternateKeyMask` is the Option/Alt key) and
///   Windows samples the live key state (`gpui_windows/src/events.rs`,
///   `current_modifiers` — `VK_MENU` is Alt), so a `mod+down` arrives
///   with `alt: true` on both platforms.
/// - A truly lost release — a move arriving with the button no longer
///   pressed — *cancels* rather than drops: the actual release point is
///   unknown, and applying the drop at wherever the cursor happens to be
///   next would rearrange from a position the user never released at.
///   (The divider catcher's missed release *finishes* instead — correct
///   there because its effects were already applied live; here nothing
///   is applied until an actual drop.) The catcher's `on_mouse_up_out`,
///   by contrast, routes through the drop like `on_mouse_up` does
///   (review blocker fix): gpui's keyboard-modality hover suppression
///   makes `up_out` fire for an ordinary in-window release whenever a
///   keystroke was the last input, and the keyboard is hot mid-drag —
///   see the catcher's own comment in `render` for the full mechanism.
///   An actually-outside-window release still applies nothing that way,
///   because no drop target exists outside every tile and dock.
#[derive(Debug, Clone, PartialEq)]
struct TileDrag {
    tile: TileId,
    epoch: u64,
    /// Window-space mouse-down position the threshold is measured from.
    origin: (f32, f32),
    /// Latest window-space cursor position — what the ghost follows and
    /// the zone highlight classifies against each frame.
    cursor: (f32, f32),
    active: bool,
}

/// Whether the configured `mod` alias's key is held in a mouse event's
/// modifier set. The alias is exactly one of `CTRL`/`ALT`/`CMD`
/// (`defaults::mod_alias_from_config` — the same source of truth the
/// keymap engine resolves `mod+` bindings through), mapped onto gpui's
/// `control`/`alt`/`platform` the same way `convert_keystroke` maps
/// keyboard modifiers. Extra held modifiers don't disqualify (matching
/// how a chorded mouse gesture is usually read); only the aliased key
/// matters.
fn mod_alias_held(alias: Modifiers, mods: &gpui::Modifiers) -> bool {
    (alias.ctrl && mods.control) || (alias.alt && mods.alt) || (alias.cmd && mods.platform)
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
    divider_drag: Option<DividerDrag>,
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
    tile_drag: Option<TileDrag>,
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

        // The dialogs' shared filter field — same lifecycle as
        // `palette_input` above (see that field's doc comment).
        let dialog_input = cx.new(|cx| InputState::new(window, cx).placeholder("filter"));
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
        let find_style = FindStyle::from_config(&services.config);

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
            dialog_input,
            desk_dir,
            user_dir,
            last_snapshot: reload::Snapshot::default(),
            last_reload: reload::ReloadOutcome::Unchanged,
            session_dirty: false,
            pending_focus_restore: false,
            divider_drag: None,
            tile_drag: None,
            filter_input,
            perf: FrameHistogram::new(),
            last_render_started: None,
            perf_overlay: false,
            data_probe: false,
            probe: crate::dataprobe::ProbeState::default(),
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
            self.find_style = FindStyle::from_config(&self.services.config);

            if theme_changed {
                self.services
                    .theme
                    .apply_from_config(&self.services.config, cx);
            }

            if palette_snapshot_changed {
                // Deliberately `self.palette = None` here, not `self.
                // close_palette(..)` (palette-input-polish task's own
                // helper, used everywhere else a close needs to hand focus
                // back to the shell root) — `apply_reload` has no `Window`
                // (it runs from the background reload watcher's plain
                // `Context<Self>` update, spec PHILOSOPHY.md: reload I/O
                // stays off the UI thread and this is the cheap synchronous
                // tail of that), so there is nothing to call `FocusHandle::
                // focus` with directly here. If the palette's `Entity<
                // InputState>` happened to hold real window focus at this
                // exact moment (a keymap/mod-alias edit landing while the
                // user is mid-query), silently dropping `self.palette`
                // would leave that `FocusId` orphaned — the dispatch tree
                // resolves an orphaned focus to its root node next frame,
                // not `ShellView`'s own `track_focus`'d div, so `handle_
                // key_down` (which lives on that div's `on_key_down`)
                // would simply stop firing: Escape, ctrl+k, hjkl, all of
                // it, dead until a mouse click claims focus somewhere else
                // first. Fix-round finding: `pending_focus_restore` below
                // is what closes that gap without needing a `Window` here.
                self.palette = None;
                self.pending_focus_restore = true;
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
    /// open. Takes `window`/`cx` (added by the palette-input-polish task,
    /// unlike the old free-text version) purely for the focus handoff:
    /// [`close_palette`](Self::close_palette) on the close arm, and, on the
    /// open arm, resetting `self.palette_input`'s value to `""`
    /// (`InputState::set_value` — checked against the pinned checkout: it
    /// does *not* emit `InputEvent::Change`, so this alone never touches
    /// `self.palette`'s query, which is already starting fresh from
    /// `PaletteState::new` a few lines below) and focusing it, so typing
    /// reaches the query field the instant the palette appears rather than
    /// requiring a click first.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
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
        self.palette_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.palette_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    /// Close the palette (if open) and hand focus back to the shell root —
    /// the "return focus to shell root on close" half of the palette-
    /// input-polish task's focus contract (the "focus it on open" half
    /// lives in `toggle_palette`'s open arm). The one standard door for
    /// closing the palette from a real key/mouse event: `toggle_palette`'s
    /// close arm, `handle_palette_key`'s escape/enter arms, the click-
    /// catcher's dismiss handler (`render`, below), and `dialog::
    /// open_shell_dialog_with_key` (a modal opening over an open palette)
    /// all go through this rather than setting `self.palette = None`
    /// directly. `apply_reload`'s own silent close is the one deliberate
    /// exception — see that call site's own comment for why (no `Window`
    /// available there).
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_handle.focus(window, cx);
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

    /// Scroll the palette's results viewport so the currently selected row
    /// is visible (`gpui::ScrollHandle::scroll_to_item`, a real per-frame
    /// layout measurement — see `palette::render`'s doc comment). Called
    /// from every path that can change `self.palette`'s selection: `handle_
    /// palette_key`'s up/down/ctrl+p/ctrl+n arms, the `InputEvent::Change`
    /// subscription set up in `new` (`PaletteState::set_query` resets the
    /// selection to row 0 on every edit, same as `push_char`/`backspace`
    /// used to — that's still a selection change the viewport must follow),
    /// and a row click (`set_selected`, via the click handler built in
    /// `render`, below). A no-op while the palette is closed.
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
        } else if action.0 == "data::toggle_probe" {
            // Display-only, like the perf overlay: the binary keeps
            // pushing readings whether or not anyone is looking.
            self.data_probe = !self.data_probe;
            cx.notify();
        } else if action.0 == "perf::reset" {
            // Zero the frame-time counters so a measurement can start
            // from a known point (e.g. right before an interaction worth
            // profiling). Notify so a visible overlay repaints its
            // zeroed numbers immediately.
            self.perf.reset();
            cx.notify();
        } else {
            // Profiler-feature actions (`perf::dump`, `perf::gpui_overlay`)
            // — compiled (and registered) only with the `profiling`
            // feature; see `shell::profiling_hook`.
            #[cfg(feature = "profiling")]
            profiling_hook::dispatch(self, action, window, cx);
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

    /// Persist the current find style to `<user_dir>/app.toml`'s `[ui]`
    /// table, off the UI thread — the exact contract of [`Self::
    /// persist_font_size`] just above (missing `user_dir` = silently
    /// skipped; failures are a stderr warning; last-write-wins races
    /// accepted for the same rare-UI-action reasons).
    fn persist_find_style(&self, cx: &mut Context<Self>) {
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

    /// Handle one *bubbled* key event while the palette is open — reworked
    /// by the palette-input-polish task from the old "owns every key,
    /// including free text entry" version. Query editing (typing,
    /// backspace/delete, caret movement, ctrl+a, ctrl+v) is no longer this
    /// method's job at all: `self.palette_input`, a real gpui-component
    /// `Input`, consumes those natively and — per the routing analysis on
    /// the `palette_input` field's own doc comment — they never reach here
    /// in the first place; `handle_key_down`'s `if self.palette.is_some()`
    /// guard (below) only ever routes here what the `Input` didn't already
    /// consume.
    ///
    /// This method now does exactly two things: act on the list of
    /// navigation/close keys the palette still owns (up/down, ctrl+p/
    /// ctrl+n, enter, escape, plus the larger steps — ctrl+d/u ±5, ctrl+f/b
    /// and pageup/pagedown ±10 — that Task 5 added as a fallback arm so
    /// this filtered list surface reads the same as the two dialogs', spec
    /// §3, "The command palette"), and otherwise do *nothing* — deliberately
    /// not `cx.stop_propagation()`, which would be the wrong kind of
    /// "swallow": a bare typed character reaches this method too (no
    /// `KeyBinding` at all matches it inside `Input`'s own "Input" context,
    /// so raw dispatch runs — see the field doc comment again), and it must
    /// keep propagating past this listener so the window's separate IME/
    /// text-input phase (`Window::dispatch_keystroke`'s second phase in
    /// tests; the platform's real text-input callback in production) still
    /// delivers it to the now-focused `palette_input`. Either way, no shell
    /// chord ever fires while the palette is open: `handle_key_down`'s own
    /// `if self.palette.is_some() { self.handle_palette_key(..); return; }`
    /// guard is a plain Rust-level branch that never falls through to
    /// `self.matcher.press` regardless of what happens in here.
    ///
    /// The named up/down/ctrl+p/ctrl+n arms below keep wrapping
    /// (`PaletteState::move_selection`, unchanged since before Task 5),
    /// while the larger steps in the fallback arm clamp
    /// (`crate::vimnav::apply`) — a page jump that teleported from the top
    /// of a long result list to the bottom would read as a glitch, not a
    /// feature. That split falls out of arm order alone: the ±1 keys
    /// return from their own arms before the fallback arm is ever reached,
    /// so nothing there has to inspect the resolved delta to pick a rule —
    /// reaching the fallback arm at all already means the key was none of
    /// those four.
    ///
    /// Reads gpui's own `Keystroke` directly (`event.keystroke`, not the
    /// shell-native one `convert_keystroke` produces) because it needs the
    /// named keys (`"up"`, `"down"`, `"enter"`, `"escape"`) and raw
    /// `modifiers` that the shell-native conversion's matcher-oriented
    /// shape doesn't carry as directly.
    fn handle_palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let mods = ks.modifiers;

        match ks.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" => {
                let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
                self.close_palette(window, cx);
                if let Some(item) = selected {
                    self.dispatch_palette_item(&item, window, cx);
                }
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
            // Everything the named arms above did not take. Two outcomes:
            // a larger navigation step (the vocabulary the two list
            // dialogs use, adopted here so all three filtered surfaces
            // read the same — spec §3, "The command palette"), or a
            // genuine no-op.
            //
            // These clamp, while the ±1 arms above wrap: reaching this
            // arm at all means the key was NOT up/down/ctrl+p/ctrl+n, so
            // nothing here has to inspect the delta to pick a rule. A
            // page jump that teleports from the top of a long result list
            // to the bottom reads as a glitch, not as a feature.
            //
            // A bare typed character lands here too, and must stay a true
            // no-op — deliberately not `cx.stop_propagation()`, so the
            // window's separate text-input phase still delivers it to the
            // focused `palette_input` (see this method's doc comment).
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

    fn handle_key_down(
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
            && self.is_palette_toggle(ks)
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

    /// Apply the active divider drag at a window-space cursor position
    /// (drag-splitters task): route to the matching pure verb with the
    /// geometry captured at mouse-down. Returns whether the layout
    /// actually changed — and since the review round that is literal: the
    /// pure verbs compare against the current value, so a stale address,
    /// a hidden dock, and a move pinned at a clamp the divider is already
    /// sitting at all report false, and the caller skips the notify (no
    /// re-render for an identical layout). `moved` latches on the first
    /// actual change (see [`DividerDrag`]'s doc for the
    /// returns-to-start caveat). Refuses — and cancels — a drag whose
    /// recorded workspace is no longer active (review fix): the render-top
    /// guard normally cancels first, but the check here is what makes the
    /// guarantee independent of event/render ordering. Pure math only —
    /// the caller notifies.
    fn apply_divider_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(epoch) = self.divider_drag.as_ref().map(|drag| drag.epoch) else {
            return false;
        };
        if epoch != self.services.workspaces.switch_epoch() {
            self.cancel_divider_drag();
            return false;
        }
        let Some(drag) = &self.divider_drag else {
            return false;
        };
        let ws = self.services.workspaces.active_mut();
        let changed = match &drag.target {
            DividerDragTarget::MainTree { address } => {
                ws.drag_main_divider(address, x, y, drag.bounds)
            }
            DividerDragTarget::DockTree { side, address } => {
                ws.drag_dock_divider(*side, address, x, y, drag.bounds)
            }
            DividerDragTarget::DockEdge { side } => ws.drag_dock_edge(*side, x, y, drag.bounds),
        };
        if changed && let Some(drag) = &mut self.divider_drag {
            drag.moved = true;
        }
        changed
    }

    /// End the active divider drag (mouse-up, wherever it lands). The
    /// session goes dirty here — once per drag, not per move — and only
    /// when the drag actually resized something, so a click-and-release
    /// on a strip writes nothing. Mirrors how keyboard resizes persist:
    /// the dirty flag coalesces onto the watcher's ~500ms background
    /// flush, never a synchronous write on the UI thread.
    fn finish_divider_drag(&mut self, cx: &mut Context<Self>) {
        self.cancel_divider_drag();
        cx.notify();
    }

    /// Drop the active drag, keeping everything it already applied
    /// (cancel means "stop tracking the mouse", never "undo") and — review
    /// fix — dirtying the session if the drag had resized anything: the
    /// first cut's cancel paths discarded `moved`, leaving a visible
    /// resize the next session restore would silently lose. Shared by the
    /// mouse-up finish, the render-top invalidation guard, and
    /// `apply_divider_drag`'s workspace check; deliberately notify-free so
    /// the render-path callers don't schedule a frame from inside one.
    fn cancel_divider_drag(&mut self) {
        if let Some(drag) = self.divider_drag.take()
            && drag.moved
        {
            self.session_dirty = true;
        }
    }

    /// Arm a pending tile drag from a tile body's mouse-down, if the
    /// gesture and the shell's state allow it (tile-drag task). Returns
    /// true when armed — the caller's plain click-to-focus branch must
    /// then NOT run (recorded decision on [`TileDrag`]: mod+down changes
    /// no focus at arm time). Refused — falling back to plain-click
    /// behavior — when the configured mod key isn't held, or in any state
    /// where a drag couldn't legitimately run: an overlay is up (the
    /// palette/modal catchers normally occlude tiles anyway — defense in
    /// depth), a keystroke sequence is pending (the which-key hint paints
    /// over the tiles, same gate as `dividers_active`), a tree tile is
    /// fullscreen (only one tile visible — nothing to rearrange; same
    /// gate that suppresses the divider strips), or another drag of
    /// either kind is already in flight (their catchers occlude tiles,
    /// so this is unreachable — but checking costs nothing and makes the
    /// exclusivity explicit).
    fn try_arm_tile_drag(
        &mut self,
        id: TileId,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !mod_alias_held(self.services.mod_alias, &event.modifiers) {
            return false;
        }
        if self.palette.is_some()
            || self.modal.is_some()
            || !self.matcher.pending().is_empty()
            || self.divider_drag.is_some()
            || self.tile_drag.is_some()
            || self
                .services
                .workspaces
                .active()
                .tree()
                .fullscreen()
                .is_some()
        {
            return false;
        }
        let position = (f32::from(event.position.x), f32::from(event.position.y));
        self.tile_drag = Some(TileDrag {
            tile: id,
            epoch: self.services.workspaces.switch_epoch(),
            origin: position,
            cursor: position,
            active: false,
        });
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// Advance the pending/active tile drag to a new cursor position
    /// (mouse-move on the tile-drag catcher). Below the movement
    /// threshold nothing visible exists yet, so no repaint is scheduled;
    /// crossing [`TILE_DRAG_THRESHOLD`] latches `active` (one-way — a
    /// drag that wanders back within 5px of its origin is still a drag),
    /// and every active-drag move repaints so the ghost and zone
    /// highlight track the cursor. Same per-move notify cost as the
    /// divider catcher's live resize — accepted while a button is held.
    fn update_tile_drag(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.tile_drag.as_mut() else {
            return;
        };
        drag.cursor = (x, y);
        if !drag.active {
            if (x - drag.origin.0).abs().max((y - drag.origin.1).abs()) < TILE_DRAG_THRESHOLD {
                return;
            }
            drag.active = true;
        }
        cx.notify();
    }

    /// Drop the in-flight tile drag with nothing applied (tile-drag task):
    /// the render-top cancel guard, a mouse-up outside the window, and a
    /// missed release all land here. Truly free — a tile drag applies
    /// nothing until its drop, so unlike `cancel_divider_drag` there is
    /// no already-applied state to keep and no session-dirty bookkeeping
    /// to do. Notify-free for the same render-path reason as its divider
    /// sibling; event-path callers notify themselves.
    fn cancel_tile_drag(&mut self) {
        self.tile_drag = None;
    }

    /// End a drag — of either kind — on a left release its own catcher
    /// could not see (post-merge review BUG 1 for the tile drag; the
    /// fix-round should-fix extended it to the divider drag, whose arm
    /// has the identical gap. Recorded choice among the three candidates:
    /// this window-level fallback, rather than arming only on a
    /// catcher-observed move or a special-case in the catcher's own up
    /// path, because it is the only one that closes the gap at its root).
    /// The gap: a tile's `on_mouse_down` (`try_arm_tile_drag`) and a
    /// strip's `on_mouse_down` (the `DividerDrag` arm) both run from
    /// listeners in the CURRENT frame, but each drag's only up/up_out
    /// listeners belong to its catcher element, which enters the hitbox
    /// tree at the NEXT paint — and gpui dispatches every queued input
    /// event between frames, so a fast click's down and up can both land
    /// before any draw. Before this fix that release hit no listener at
    /// all and the armed drag survived indefinitely: the next frame
    /// painted the full-window catcher (grabbing cursor for tiles,
    /// resize cursor for dividers), the user's next stationary
    /// mouse-down was swallowed (neither catcher registers a down
    /// handler), and an unmodified press-drag could be APPLIED — a
    /// rearrangement with no mod key held, or a live resize of a divider
    /// the user never grabbed.
    ///
    /// Called from a pair of listeners on the root element (`render`),
    /// which is painted every frame: `on_mouse_up` (bubble, hovered) and
    /// `on_mouse_up_out` (capture, not-hovered) — between them every
    /// left release reaches this method, for BOTH drag kinds, across all
    /// four modality×state combinations:
    /// - gap frame (no catcher painted), mouse modality: the root is
    ///   hovered, its bubble `on_mouse_up` fires — the phantom clears;
    /// - gap frame, keyboard modality: nothing is hovered
    ///   (`HitboxId::is_hovered` is false under keyboard modality), the
    ///   root's capture `on_mouse_up_out` fires — same heal;
    /// - post-paint with a catcher up, mouse modality: the occluding
    ///   catcher removes the root from the hover chain, so the root's
    ///   `up_out` fires (capture phase — BEFORE the catcher's own bubble
    ///   `on_mouse_up`);
    /// - post-paint, keyboard modality: both the root's and the
    ///   catcher's `up_out` fire in capture phase, root (outermost)
    ///   first.
    ///
    /// In the two post-paint cases this method acting FIRST must not
    /// change what the catcher's own release path would have done, and
    /// the two drag kinds guarantee that differently:
    /// - tile drag: guarded to the never-activated state only — an
    ///   ACTIVE drag's release must keep routing through the catcher's
    ///   `finish_tile_drag` drop path, so an early cancel here would eat
    ///   real drops. A drag can only be active once a catcher-observed
    ///   move latched it (a catcher exists only after a paint), so in
    ///   the gap frame the guard is always met, and after a paint the
    ///   cancel equals the below-threshold no-op finish the catcher
    ///   would have performed anyway.
    /// - divider drag: cancelled UNconditionally, safe because for
    ///   dividers finish IS cancel-plus-notify — every resize was
    ///   already applied live by the moves, the catcher's own up applies
    ///   nothing positional, and `cancel_divider_drag` keeps the applied
    ///   state and dirties the session iff the drag moved. (It has no
    ///   `active` latch to guard on, and needs none.) The catcher's
    ///   subsequent finish finds the drag already gone and no-ops.
    fn heal_drags_on_root_release(&mut self, cx: &mut Context<Self>) {
        let heal_tile = self.tile_drag.as_ref().is_some_and(|drag| !drag.active);
        if heal_tile {
            self.cancel_tile_drag();
        }
        let heal_divider = self.divider_drag.is_some();
        if heal_divider {
            self.cancel_divider_drag();
        }
        if heal_tile || heal_divider {
            cx.notify();
        }
    }

    /// Apply the drop that ends a tile drag (mouse-up on the tile-drag
    /// catcher). A below-threshold drag — a sloppy mod+click — applies
    /// nothing at all, focus included (recorded decision on [`TileDrag`]).
    /// The full set of render-top cancel conditions is re-checked at drop
    /// time too — workspace mismatch, palette/modal open, pending
    /// keystroke sequence (review should-fix: gpui dispatches multiple
    /// input events between frames, so `ctrl+k` followed by the release
    /// within one frame window would otherwise apply the drop underneath
    /// the just-opened palette; the render-top guard normally cancels
    /// first, but re-checking here keeps the guarantee independent of
    /// event/render ordering, the same standard `apply_divider_drag`'s
    /// workspace check sets). Fullscreen needs no shell-side re-check:
    /// [`locate_drop_target`] itself resolves no target for a fullscreen
    /// layout. Otherwise the
    /// cursor resolves through the pure [`locate_drop_target`] against
    /// *live* state — the same `dock_layout`/`Tree::layout` authorities
    /// the render pass uses, re-derived once here rather than snapshotted
    /// at mouse-down, so a keyboard split mid-drag can't make the drop
    /// land beside a tile the user isn't seeing — and routes to the
    /// matching pure `Workspace` drop verb: center → swap, edge → split-
    /// insert, dock background → move-to-dock-convention insert. The
    /// verbs own every focus/region/auto-hide rule and report whether the
    /// layout changed; only a real change dirties the session (no-op
    /// drops — self-drops, a release over nothing — don't).
    fn finish_tile_drag(&mut self, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.tile_drag.take() else {
            return;
        };
        if drag.active
            // Epoch, not index (post-merge review finding 7): index
            // equality let a switch-away-and-back with no render between
            // satisfy the letter of the re-check — the epoch bumps on
            // every actual switch, so it only matches when no switch
            // happened at all.
            && drag.epoch == self.services.workspaces.switch_epoch()
            && self.palette.is_none()
            && self.modal.is_none()
            && self.matcher.pending().is_empty()
            // The dragged tile must still exist (post-merge review BUG 3
            // — same event/render-ordering independence as the checks
            // above: ctrl+w and the release can land in one frame
            // window). The drop verbs would refuse a vanished id anyway;
            // checking here keeps the shared cancel conditions one list.
            && self
                .services
                .workspaces
                .active()
                .region_of(drag.tile)
                .is_some()
        {
            let viewport = window.viewport_size();
            let toolbar_height = f32::from(TITLE_BAR_HEIGHT);
            let area = Rect {
                x: 0.0,
                y: 0.0,
                w: (f32::from(viewport.width) - sidebar::WIDTH).max(0.0),
                h: (f32::from(viewport.height) - toolbar_height - status::HEIGHT).max(0.0),
            };
            // Mouse events arrive in window coordinates; the tile surface
            // starts below the toolbar, right of the sidebar (same
            // conversion `render` bakes into its drag rects).
            let sx = x - sidebar::WIDTH;
            let sy = y - toolbar_height;
            let target = locate_drop_target(self.services.workspaces.active(), area, sx, sy);
            let ws = self.services.workspaces.active_mut();
            let changed = match target {
                Some(DropTarget::Tile {
                    id,
                    zone: DropZone::Center,
                }) => ws.drop_swap(drag.tile, id),
                Some(DropTarget::Tile {
                    id,
                    zone: DropZone::Edge(edge),
                }) => ws.drop_split(drag.tile, id, edge),
                Some(DropTarget::DockBackground { side }) => ws.drop_to_dock(drag.tile, side),
                None => false,
            };
            if changed {
                self.session_dirty = true;
            }
        }
        cx.notify();
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Frame-time instrumentation (spec §7.4), first thing so the
        // interval is measured from the true top of each render. Records
        // the render-to-render interval — see `crate::perf`'s module doc
        // for exactly what this signal captures (consecutive renders
        // during interaction bursts) and doesn't (compositor time, the
        // last frame before idleness). O(1), allocation-free, and it
        // never notifies or schedules anything, so recording can't force
        // a frame; intervals >= IDLE_CUTOFF are counted as idle gaps,
        // not frames.
        let render_started = std::time::Instant::now();
        if let Some(prev) = self.last_render_started {
            let interval = render_started.saturating_duration_since(prev);
            if interval < perf::IDLE_CUTOFF {
                self.perf.record_micros(interval.as_micros() as u64);
            } else {
                self.perf.note_discarded_idle();
            }
        }
        self.last_render_started = Some(render_started);

        // Fix-round finding: consume a pending focus restore left by a
        // background path that closed the palette with no `Window` in hand
        // (see `pending_focus_restore`'s and `apply_reload`'s own doc
        // comments for the orphaned-`FocusId` bug this closes). `render`
        // is the first point downstream that actually has a `&mut Window`
        // — and the *only* point that will reliably run at all in the
        // failure state being fixed, since an orphaned focus is exactly
        // what makes `handle_key_down` stop firing until a mouse click
        // claims focus elsewhere first. Kept ahead of everything else in
        // render, per its contract.
        if self.pending_focus_restore {
            self.pending_focus_restore = false;
            self.focus_handle.focus(window, cx);
        }

        // Cancel an in-flight divider drag when the surface it was
        // resizing is no longer the one on screen (drag-splitters task):
        // the palette or a modal opened mid-drag (keyboard stays live
        // during a drag — ctrl+k works with the button held), a tree tile
        // went fullscreen (mod+f likewise), or mod+N switched to another
        // workspace (review fix: the recorded address and bounds belong to
        // the workspace the drag started in; without this, the next move
        // would apply them to the new workspace's tree). All of these hide
        // the dragged boundary, and letting the drag keep mutating an
        // invisible layout would be a surprise on return. Cancel means
        // "stop tracking the mouse", NOT "undo": whatever the drag already
        // applied stays applied and — review fix — still persists like
        // any resize (`cancel_divider_drag` dirties the session when the
        // drag had moved; the first cut silently dropped that, leaving a
        // visible layout the next restore wouldn't reproduce). Same
        // consume-state-at-the-top-of-render precedent as
        // `pending_focus_restore` just above: render is the one place
        // every one of those paths reliably funnels through with the
        // state fresh.
        if self.divider_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal.is_some()
                // Epoch, not index (finding 7) — see `DividerDrag::epoch`.
                || drag.epoch != self.services.workspaces.switch_epoch()
                || self
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .fullscreen()
                    .is_some()
        }) {
            self.cancel_divider_drag();
        }

        // Cancel an in-flight tile drag on the same conditions (tile-drag
        // task) — palette/modal opened, workspace switched, fullscreen
        // toggled — PLUS a which-key hint appearing: unlike a divider
        // drag (which keeps its already-applied resize live behind the
        // handler-less hint), a tile drag is all about *choosing a drop
        // target among the tiles*, and doing that under a panel that
        // covers part of them would be blind targeting. PLUS (post-merge
        // review BUG 3) the dragged tile no longer existing anywhere:
        // ctrl+w can close it mid-drag (the keyboard stays hot), and
        // without this check the ghost and zone highlight kept painting
        // — promising a drop the verbs would silently refuse. All of
        // these make cancelling truly free here: a tile drag applies
        // nothing until its drop, so cancel undoes nothing, dirties
        // nothing, and needs none of the divider guard's `moved`
        // bookkeeping.
        if self.tile_drag.as_ref().is_some_and(|drag| {
            self.palette.is_some()
                || self.modal.is_some()
                || !self.matcher.pending().is_empty()
                // Epoch, not index (finding 7) — see `DividerDrag::epoch`.
                || drag.epoch != self.services.workspaces.switch_epoch()
                || self
                    .services
                    .workspaces
                    .active()
                    .tree()
                    .fullscreen()
                    .is_some()
                || self
                    .services
                    .workspaces
                    .active()
                    .region_of(drag.tile)
                    .is_none()
        }) {
            self.cancel_tile_drag();
        }

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

        // One layout pass for the whole surface (dock-regions task,
        // generalized by dock-trees): the pure `tiling::dock_layout`
        // carves the visible docks' pixel rects out of the content area,
        // `Tree::layout` partitions what's left for the main tree, and
        // each visible dock's rect feeds that dock's own `Tree::layout` —
        // one geometry call per region (main + up to three visible docks),
        // still a single pass overall. While a tree tile is fullscreen it
        // covers the entire surface and the docks are not painted at all
        // (the docks keep their state; they're just not part of the
        // fullscreen picture).
        let area = Rect {
            x: 0.0,
            y: 0.0,
            w: tile_width,
            h: content_height,
        };
        type DockCell = (
            crate::tiling::DockSide,
            Rect,
            Vec<(crate::tiling::TileId, Rect)>,
            Option<crate::tiling::TileId>,
        );
        // Divider strips are painted (and their listeners armed) only
        // while no overlay is up: the palette's click-catcher and the
        // modal's backdrop both cover the whole window ABOVE the strips
        // but without occluding them, so a live strip underneath would
        // still take the same mouse-down that dismisses the overlay and
        // start a drag from under it. The which-key hint (review fix) is
        // the same leak in miniature: `whichkey::render` paints a solid
        // panel with no `.occlude()` and no handlers, so a mouse-down
        // inside its bounds would fall straight through to a strip
        // beneath — it shows exactly while a keystroke sequence is
        // pending, so that state gates too. (An already-in-flight drag is
        // NOT cancelled for which-key the way palette/modal cancel it:
        // the hint has no mouse handlers to fight the drag catcher, and
        // the dragged boundary stays visible behind it.) Gating at paint
        // time keeps the rule simple: strips exist exactly when the tiles
        // they resize are the frontmost interactive surface.
        let dividers_active =
            self.palette.is_none() && self.modal.is_none() && self.matcher.pending().is_empty();
        // Mouse events arrive in window coordinates while the tile
        // geometry lives in surface coordinates (the surface starts below
        // the toolbar, right of the sidebar) — the drag rects captured at
        // mouse-down are pre-offset into window space so the per-move math
        // never converts.
        let to_window_space = |r: Rect| Rect {
            x: r.x + sidebar::WIDTH,
            y: r.y + toolbar_height,
            w: r.w,
            h: r.h,
        };
        let (region, focused, tree_area, rects, dock_cells, strips) = {
            let workspace = self.services.workspaces.active();
            let tree = workspace.tree();
            let (tree_area, dock_rects) = if tree.fullscreen().is_some() {
                (area, Vec::new())
            } else {
                crate::tiling::dock_layout(workspace.docks(), area)
            };
            // The frame's divider strips, from the same rects this pass
            // just computed (drag-splitters task): the main tree's
            // interior boundaries, each visible dock tree's interior
            // boundaries, then the dock frame edges — edges last so they
            // paint above a dock tree's own strips where the two meet at
            // a corner (hit-testing follows paint order). `divider_strips`
            // itself yields nothing for a fullscreen tree, and
            // `dock_rects` is already empty then, so fullscreen suppresses
            // every strip without a separate check here.
            let mut strips: Vec<StripSpec> = Vec::new();
            if dividers_active {
                let tree_bounds = to_window_space(tree_area);
                for s in divider_strips(tree, tree_area, DIVIDER_HIT_WIDTH) {
                    strips.push(StripSpec {
                        rect: s.rect,
                        axis: s.orientation,
                        target: DividerDragTarget::MainTree { address: s.address },
                        drag_bounds: tree_bounds,
                    });
                }
                for &(side, r) in &dock_rects {
                    let dock_bounds = to_window_space(r);
                    for s in
                        divider_strips(workspace.docks().get(side).tree(), r, DIVIDER_HIT_WIDTH)
                    {
                        strips.push(StripSpec {
                            rect: s.rect,
                            axis: s.orientation,
                            target: DividerDragTarget::DockTree {
                                side,
                                address: s.address,
                            },
                            drag_bounds: dock_bounds,
                        });
                    }
                }
                let area_bounds = to_window_space(area);
                for e in dock_edge_strips(&dock_rects, DIVIDER_HIT_WIDTH) {
                    strips.push(StripSpec {
                        rect: e.rect,
                        axis: e.orientation,
                        target: DividerDragTarget::DockEdge { side: e.side },
                        drag_bounds: area_bounds,
                    });
                }
            }
            // Each visible dock carries its own tile layout plus its
            // tree's focused tile (the ring shows on the focused dock's
            // focused tile only — still at most one ring per workspace,
            // region-gated below).
            let dock_cells: Vec<DockCell> = dock_rects
                .into_iter()
                .map(|(side, r)| {
                    let dock_tree = workspace.docks().get(side).tree();
                    (side, r, dock_tree.layout(r), dock_tree.focused())
                })
                .collect();
            (
                workspace.region(),
                tree.focused(),
                tree_area,
                tree.layout(tree_area),
                dock_cells,
                strips,
            )
        };

        // The active drop target's zone highlight (tile-drag task): the
        // SAME resolution core the drop itself uses
        // (`tiling::resolve_drop_target` — post-merge review cleanup 8:
        // the first cut restated the dock-frame → dock-tile →
        // dock-background → tree-tile order here by hand, and two copies
        // of a targeting rule is how a highlight drifts from the drop it
        // promises), fed the rects the single layout pass above just
        // produced — no second layout, no I/O, and no allocation (the
        // core takes a borrowed iterator), per the render-discipline
        // constraint. An edge zone highlights the half of the target tile
        // the insert would occupy, center the whole tile, a dock
        // background the dock's frame. What stays HERE, on top of the
        // core, is the Workspace-side no-op filtering — targets whose
        // drop the verbs would refuse paint nothing, because
        // highlighting them would promise a rearrangement that won't
        // happen: the dragged tile itself (self-drops are recorded
        // no-ops for every zone), and the background of the dock the
        // tile already lives in (defensive — an occupied dock's tiles
        // cover its whole frame, so this is unreachable in practice).
        let drop_highlight: Option<Rect> = self
            .tile_drag
            .as_ref()
            .filter(|drag| drag.active)
            .and_then(|drag| {
                let sx = drag.cursor.0 - sidebar::WIDTH;
                let sy = drag.cursor.1 - toolbar_height;
                let dragged = drag.tile;
                let target = crate::tiling::resolve_drop_target(
                    dock_cells
                        .iter()
                        .map(|(side, r, tiles, _)| (*side, *r, tiles.as_slice())),
                    &rects,
                    sx,
                    sy,
                )?;
                match target {
                    (DropTarget::Tile { id, .. }, _) if id == dragged => None,
                    (DropTarget::Tile { id: _, zone }, tr) => Some(drop_highlight_rect(tr, zone)),
                    (DropTarget::DockBackground { side }, frame) => {
                        let already_here = self
                            .services
                            .workspaces
                            .active()
                            .docks()
                            .get(side)
                            .tree()
                            .contains(dragged);
                        (!already_here).then_some(frame)
                    }
                }
            });

        // The shared tile chrome — identical for tree tiles and docked
        // tiles (a docked tile is the same kind of tile, just parked): 1px
        // inset, themed background, `primary` 2px ring on the one focused
        // tile, mono placeholder label. At most one tile per workspace
        // shows the focused ring: the main tree's focused tile only counts
        // as focused while `region == Main`, and a dock tile only when
        // focus lives in that dock AND the dock's own tree has it focused
        // (dock-trees task — a dock holds many tiles, one ring).
        let tile_cell = |id: crate::tiling::TileId, r: Rect, is_focused: bool, cx: &App| {
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
                .child(format!("tile {}", id.0))
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
            // The empty hint fills the *tree's* remaining area (not the
            // whole surface — visible docks keep their columns), so it is
            // its own absolutely-positioned, internally-centered child
            // rather than turning the surface itself into a flex row.
            //
            // State-aware (review nit): while a dock holds focus, a split
            // lands in the *dock's* tree (dock-trees task), so advertising
            // ctrl+h/ctrl+v as the way to fill the empty main area would
            // be misleading advice; name the move-back chord for the
            // focused dock instead, in the same physical-key spelling the
            // dock hints use (the user presses ctrl+shift+[; the binding
            // is spelled `ctrl+{` — see BUILTIN_KEYMAP's doc comment).
            // "the focused docked tile": a dock can hold several tiles
            // now, and the chord moves exactly the one its tree has
            // focused, one per press.
            let (hint, selector) = match region {
                crate::tiling::FocusRegion::Main => {
                    ("ctrl+h / ctrl+v to open a tile", "empty-hint")
                }
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Left) => (
                    "ctrl+shift+[ moves the focused docked tile back here",
                    "empty-hint-return-left",
                ),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Right) => (
                    "ctrl+shift+] moves the focused docked tile back here",
                    "empty-hint-return-right",
                ),
                crate::tiling::FocusRegion::Dock(crate::tiling::DockSide::Bottom) => (
                    "ctrl+shift+/ moves the focused docked tile back here",
                    "empty-hint-return-bottom",
                ),
            };
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(tree_area.x))
                    .top(px(tree_area.y))
                    .w(px(tree_area.w))
                    .h(px(tree_area.h))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            // Test-only hook (no-op outside test/test-support
                            // builds): lets a `#[gpui::test]` confirm this branch
                            // actually painted via `VisualTestContext::debug_bounds`
                            // — gpui's test API has no way to inspect painted text
                            // content itself, so this is the closest honest check
                            // available for "the hint painted".
                            .debug_selector(|| selector.to_string())
                            .text_color(cx.theme().muted_foreground)
                            .child(hint),
                    ),
            );
        } else {
            for (id, r) in rects {
                let is_focused = region == crate::tiling::FocusRegion::Main && focused == Some(id);
                surface = surface.child(
                    tile_cell(id, r, is_focused, cx)
                        // Click-to-focus is a convenience: keyboard (hjkl)
                        // remains the primary path through the same
                        // `Workspace::focus_main_tile` seam — a click on a
                        // tree tile also returns the region to Main, and
                        // both region and focus persist, so the session
                        // goes dirty like any workspace-mutating dispatch.
                        // With the configured mod key held, the same
                        // mouse-down instead arms a pending tile drag
                        // (tile-drag task) — and deliberately does NOT
                        // focus: see `try_arm_tile_drag` / `TileDrag`.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                                if view.try_arm_tile_drag(id, event, cx) {
                                    return;
                                }
                                if view.services.workspaces.active_mut().focus_main_tile(id) {
                                    view.session_dirty = true;
                                }
                                cx.notify();
                            }),
                        ),
                );
            }
        }

        // The docks, painted with the identical chrome (dock-trees task:
        // each visible dock lays its own tree's tiles into its frame —
        // same `tile_cell`, click-to-focus included; a click focuses that
        // tile *within* the dock's tree AND moves the region there). A
        // visible but empty dock renders a centered muted hint naming the
        // *physical* keys that would move a tile into it (the user presses
        // ctrl+shift+[ even though the binding is spelled `ctrl+{` — see
        // BUILTIN_KEYMAP's doc comment).
        for (side, r, dock_tiles, dock_focused) in dock_cells {
            if !dock_tiles.is_empty() {
                for (id, tr) in dock_tiles {
                    let is_focused = region == crate::tiling::FocusRegion::Dock(side)
                        && dock_focused == Some(id);
                    // Same mod+down drag-arming branch as the tree tiles
                    // above — a docked tile is the same kind of tile, and
                    // drags work from any source region.
                    surface = surface.child(tile_cell(id, tr, is_focused, cx).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                            if view.try_arm_tile_drag(id, event, cx) {
                                return;
                            }
                            if view
                                .services
                                .workspaces
                                .active_mut()
                                .focus_dock_tile(side, id)
                            {
                                view.session_dirty = true;
                            }
                            cx.notify();
                        }),
                    ));
                }
            } else {
                let (hint, selector) = match side {
                    crate::tiling::DockSide::Left => {
                        ("ctrl+shift+[ moves a tile here", "dock-empty-hint-left")
                    }
                    crate::tiling::DockSide::Right => {
                        ("ctrl+shift+] moves a tile here", "dock-empty-hint-right")
                    }
                    crate::tiling::DockSide::Bottom => {
                        ("ctrl+shift+/ moves a tile here", "dock-empty-hint-bottom")
                    }
                };
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
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_color(cx.theme().muted_foreground)
                        .child(div().debug_selector(|| selector.to_string()).child(hint)),
                );
            }
        }

        // The divider strips (drag-splitters task), painted after — so
        // above — every tile and dock cell: transparent hit areas
        // `DIVIDER_HIT_WIDTH` wide centered on each draggable boundary,
        // each carrying a 2px line that lights up `primary` on hover (and
        // stays lit on the strip being dragged, whose cursor may be far
        // away mid-drag). `.occlude()` is what makes a strip's mouse-down
        // win over the click-to-focus listener of the tile edges it
        // overlaps: an occluding hitbox removes everything painted below
        // it from the hover chain, so the tile's own `on_mouse_down`
        // (hover-gated by gpui) never fires — same mechanism
        // gpui-component's `ResizeHandle` relies on. The mouse-down only
        // *records* the drag; the moves are handled by the full-window
        // drag catcher near the end of this method, because a fast drag
        // leaves this thin strip immediately (the capture problem).
        for (i, spec) in strips.into_iter().enumerate() {
            let StripSpec {
                rect: r,
                axis,
                target,
                drag_bounds,
            } = spec;
            let is_active = self
                .divider_drag
                .as_ref()
                .is_some_and(|drag| drag.target == target);
            let line = div()
                .group_hover(DIVIDER_GROUP, |s| s.bg(cx.theme().primary))
                .when(is_active, |el| el.bg(cx.theme().primary))
                .map(|el| match axis {
                    Orientation::Horizontal => el.w(px(2.0)).h_full(),
                    Orientation::Vertical => el.h(px(2.0)).w_full(),
                });
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(r.x))
                    .top(px(r.y))
                    .w(px(r.w))
                    .h(px(r.h))
                    .occlude()
                    .group(DIVIDER_GROUP)
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|el| match axis {
                        Orientation::Horizontal => el.cursor_col_resize(),
                        Orientation::Vertical => el.cursor_row_resize(),
                    })
                    // Test-only hook (no-op outside test builds), same
                    // honest-limitation story as the empty hints above:
                    // lets a `#[gpui::test]` confirm strips painted (or
                    // didn't — fullscreen) via `debug_bounds`.
                    .debug_selector(|| format!("divider-strip-{i}"))
                    // Recorded interaction with the tile-drag gesture: a
                    // mod+mouse-down landing within the 8px strip starts a
                    // divider RESIZE, never a tile drag — the strip
                    // occludes the tile body it overlaps and this handler
                    // checks no modifiers. Deterministic and accepted: a
                    // mod+drag aimed within ~4px of a tile's edge grabs
                    // the divider instead of the tile.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _event, _window, cx| {
                            view.divider_drag = Some(DividerDrag {
                                target: target.clone(),
                                bounds: drag_bounds,
                                axis,
                                // Pin the workspace era the drag belongs
                                // to (read at mouse-down, not paint): a
                                // mod+N switch mid-drag cancels rather
                                // than retargeting — see `DividerDrag`.
                                epoch: view.services.workspaces.switch_epoch(),
                                moved: false,
                            });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(line),
            );
        }

        // The zone highlight (tile-drag task), painted after — so above —
        // every tile, dock cell, and divider strip: a translucent
        // theme-primary wash over exactly the area the drop would occupy.
        // Instant, no animation; no handlers and no `.occlude()`, so it
        // never competes for the mouse events the tile-drag catcher below
        // owns. `primary.opacity(0.2)` is the established translucent-
        // accent pattern (sidebar workspace pill, keybindings match
        // highlight), not a raw color.
        if let Some(hr) = drop_highlight {
            surface = surface.child(
                div()
                    .absolute()
                    .left(px(hr.x))
                    .top(px(hr.y))
                    .w(px(hr.w))
                    .h(px(hr.h))
                    .bg(cx.theme().primary.opacity(0.2))
                    // Test hook, same honest-limitation story as the
                    // divider strips': painted-or-not via `debug_bounds`.
                    .debug_selector(|| "tile-drop-highlight".to_string()),
            );
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

        // Extracted ahead of the render chain like `modal` above: all the
        // drag catcher below needs from the active drag is which resize
        // cursor to show — the drag itself is applied through
        // `apply_divider_drag`, which re-reads `self.divider_drag` per
        // event.
        let drag_axis = self.divider_drag.as_ref().map(|drag| drag.axis);

        // Extracted the same way for the tile-drag catcher and ghost: the
        // catcher exists from arm (so it can see the threshold-crossing
        // moves), the ghost only once the drag is active.
        let tile_drag_armed = self.tile_drag.is_some();
        let tile_drag_ghost = self
            .tile_drag
            .as_ref()
            .filter(|drag| drag.active)
            .map(|drag| drag.cursor);

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            // Root-level left-release fallback (post-merge review BUG 1,
            // extended to divider drags by the fix-round should-fix):
            // ends a drag of either kind whose release landed in the
            // arm-to-first-paint gap, before its catcher's own up
            // handlers exist — see `heal_drags_on_root_release`'s doc
            // comment for the full mechanism, the four modality×state
            // combinations, and why BOTH listeners are needed
            // (`on_mouse_up` is bubble-phase and hover-gated,
            // `on_mouse_up_out` capture-phase and NOT-hovered-gated;
            // input modality and occluding overlays flip which one
            // fires, and together they cover every left release).
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _event, _window, cx| {
                    view.heal_drags_on_root_release(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _event, _window, cx| {
                    view.heal_drags_on_root_release(cx);
                }),
            )
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(toolbar)
            .child(body)
            .child(status_bar)
            // The divider drag catcher (drag-splitters task): while a drag
            // is active, a transparent full-window layer above the tiles
            // and status bar (but below the palette/modal overlays, which
            // cancel drags anyway — see the guard at the top of `render`)
            // owns every mouse-move and mouse-up until the button is
            // released. This is the capture mechanism: a fast drag leaves
            // the thin strip immediately, and gpui's element-level
            // `on_mouse_move` is hover-gated — but this layer's hitbox IS
            // the whole window, so hover-gating is satisfied wherever the
            // cursor goes (the div-composition equivalent of the
            // window-level `window.on_mouse_event` listeners Zed's own
            // pane-resize custom element registers in paint). `.occlude()`
            // also suppresses tile hover/click behavior for the drag's
            // duration, and the layer carries the axis resize cursor so
            // the pointer keeps its col/row-resize shape even while it's
            // off the strip — the same effect as Zed's
            // `set_window_cursor_style` during a handle drag. Mouse-up out
            // of the window (capture-phase `on_mouse_up_out`) and a move
            // arriving with the button no longer pressed (a missed
            // release) both end the drag too, so it can never get stuck.
            .when_some(drag_axis, |el, axis| {
                el.child(
                    div()
                        .id("divider-drag-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .occlude()
                        .map(|el| match axis {
                            Orientation::Horizontal => el.cursor_col_resize(),
                            Orientation::Vertical => el.cursor_row_resize(),
                        })
                        .debug_selector(|| "divider-drag-catcher".to_string())
                        .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                            // Post-merge review BUG 4 (platform-uniform
                            // rule, shared with the tile catcher below —
                            // see its comment for the verified platform
                            // evidence): only a BUTTONLESS move is the
                            // lost-release finish; a move reporting a
                            // non-Left button is a chorded second button
                            // and is ignored entirely.
                            match event.pressed_button {
                                None => {
                                    // The release happened where we
                                    // couldn't see it — treat the first
                                    // buttonless move as the mouse-up.
                                    view.finish_divider_drag(cx);
                                }
                                Some(MouseButton::Left) => {
                                    if view.apply_divider_drag(
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                    ) {
                                        cx.notify();
                                    }
                                }
                                Some(_) => {}
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, _event, _window, cx| {
                                view.finish_divider_drag(cx);
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|view, _event, _window, cx| {
                                view.finish_divider_drag(cx);
                            }),
                        ),
                )
            })
            // The tile-drag catcher (tile-drag task): the same full-window
            // capture mechanism as the divider catcher above — while a
            // drag is armed or active, a transparent occluding layer owns
            // every mouse-move and the release, wherever the cursor goes.
            // Its `.occlude()` is also what keeps the divider strips (and
            // tile click-to-focus, and strip hover styling) from fighting
            // an in-flight tile drag: the catcher is painted after the
            // whole tile surface, so everything under it leaves the hover
            // chain for the drag's duration. The two catchers can never
            // coexist — each drag kind's mouse-down is unreachable while
            // the other's catcher occludes the window (and
            // `try_arm_tile_drag` checks anyway). A move arriving with
            // the button no longer pressed cancels with nothing applied
            // — see `TileDrag`'s doc for why that differs from the
            // divider catcher's finish. `on_mouse_up_out` routes through
            // the DROP, not a cancel (review blocker fix): gpui's
            // input-modality hover suppression means it fires for a
            // perfectly ordinary in-window release whenever a keystroke
            // was the last input — a KeyDown sets the window's
            // `last_input_modality` to Keyboard (pinned window.rs,
            // `dispatch_event`), a MouseUp does NOT reset it, and
            // `HitboxId::is_hovered` returns false under keyboard
            // modality, which flips the hovered-gated `on_mouse_up` off
            // and the `!is_hovered`-gated `on_mouse_up_out` on. The
            // keyboard is documented hot mid-drag, so "press any key,
            // release without moving" is a real user path and must drop,
            // not silently cancel. A genuinely outside-window release
            // still applies nothing through this route:
            // `locate_drop_target` has no target at a position outside
            // every tile and dock, and a no-target drop is a no-op.
            .when(tile_drag_armed, |el| {
                el.child(
                    div()
                        .id("tile-drag-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .occlude()
                        .cursor_grabbing()
                        .debug_selector(|| "tile-drag-catcher".to_string())
                        .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _window, cx| {
                            // Post-merge review BUG 4 (recorded decision +
                            // platform evidence): the old
                            // `pressed_button != Some(Left)` test made
                            // this catcher platform-divergent. macOS
                            // translates NSRightMouseDragged /
                            // NSOtherMouseDragged into MouseMoveEvents
                            // whose `pressed_button` is that button
                            // (`gpui_macos/src/events.rs`, the
                            // `*MouseDragged` arm — buttonNumber mapped
                            // verbatim, no left-first normalization), so
                            // pressing a second button mid-drag CANCELLED
                            // here; Windows' WM_MOUSEMOVE translation
                            // (`gpui_windows/src/events.rs`,
                            // `handle_mouse_move_msg`) checks MK_LBUTTON
                            // first, so the same chord SURVIVED there.
                            // Unified rule: only a BUTTONLESS move is the
                            // lost-release cancel (every platform reports
                            // `None` once all buttons are up); a move
                            // reporting a non-Left button is a chorded
                            // second press and is IGNORED — it neither
                            // advances the drag (its position belongs to
                            // another button's stream) nor cancels it.
                            match event.pressed_button {
                                None => {
                                    view.cancel_tile_drag();
                                    cx.notify();
                                }
                                Some(MouseButton::Left) => {
                                    view.update_tile_drag(
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                        cx,
                                    );
                                }
                                Some(_) => {}
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, event: &MouseUpEvent, window, cx| {
                                view.finish_tile_drag(
                                    f32::from(event.position.x),
                                    f32::from(event.position.y),
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|view, event: &MouseUpEvent, window, cx| {
                                view.finish_tile_drag(
                                    f32::from(event.position.x),
                                    f32::from(event.position.y),
                                    window,
                                    cx,
                                );
                            }),
                        ),
                )
            })
            // The drag ghost (tile-drag task): a lightweight fixed-size
            // outline following the cursor — deliberately NOT a copy of
            // the tile content (see TILE_DRAG_GHOST_SIZE's recorded
            // choice). Painted above the catcher; a plain div with no
            // handlers and no `.occlude()`, so it can never swallow the
            // catcher's events even when the cursor overlaps it.
            .when_some(tile_drag_ghost, |el, (gx, gy)| {
                el.child(
                    div()
                        .absolute()
                        .left(px(gx + TILE_DRAG_GHOST_OFFSET))
                        .top(px(gy + TILE_DRAG_GHOST_OFFSET))
                        .w(px(TILE_DRAG_GHOST_SIZE.0))
                        .h(px(TILE_DRAG_GHOST_SIZE.1))
                        .border_2()
                        .border_color(cx.theme().primary)
                        .debug_selector(|| "tile-drag-ghost".to_string()),
                )
            })
            // The throwaway data probe (spec §7), painted above the tiles
            // but *below* the palette, the modal and which-key.
            //
            // Unlike the perf overlay — small, top-right, and painted above
            // everything so it can measure the layers it sits over — this
            // panel is full-window and opaque. Painted last it covered the
            // palette completely, so the one route to `data::toggle_probe`
            // that does not need the keybinding was invisible: the palette
            // was open and taking keys, and nothing on screen said so. A
            // diagnostic must not be able to hide the way out of itself.
            .when(self.data_probe, |el| {
                el.child(crate::dataprobe::render(&self.probe, toolbar_height, cx))
            })
            // The palette overlay paints above the tiles/status bar (later
            // children paint above earlier siblings) but below gpui-
            // component's own dialog/notification layers below.
            .when_some(self.palette.as_ref(), |el, state| {
                // Row click -> select (no dispatch — Enter still
                // dispatches, via `handle_palette_key`): a small `Clone`-
                // able closure over a `WeakEntity<Self>`, not `cx.listener`
                // directly (its returned `impl Fn` isn't itself `Clone`,
                // and `palette::render` clones this once per row to close
                // over each row's own index — see that function's doc
                // comment) — this way building it costs one stack closure,
                // not a heap allocation per row, per frame, while the
                // palette is open (PHILOSOPHY.md: "per-frame heap churn is
                // a defect").
                let weak = cx.entity().downgrade();
                let on_row_click = move |idx: usize, _window: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(palette) = view.palette.as_mut() {
                            palette.set_selected(idx);
                        }
                        view.sync_palette_scroll();
                        cx.notify();
                    });
                };
                let panel = palette::render(
                    state,
                    &self.palette_scroll,
                    &self.palette_input,
                    on_row_click,
                    width,
                    viewport_height,
                    cx,
                );
                // Click-outside dismiss: a transparent (no dimming — the
                // palette is an overlay, not a modal) full-window click-
                // catcher behind the panel. Precedent: `dialog::
                // render_modal`'s own backdrop, minus the `.bg(overlay)`
                // dimming a real modal wants and this doesn't. The panel
                // itself stops propagation on its own `on_mouse_down` (see
                // `palette::render`'s doc comment), so a click landing
                // anywhere inside it — a row, the query input, empty space
                // — never also reaches this catcher's handler below.
                el.child(
                    div()
                        .id("palette-click-catcher")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(width))
                        .h(px(viewport_height))
                        .debug_selector(|| "palette-click-catcher".to_string())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _event, window, cx| {
                                view.close_palette(window, cx);
                            }),
                        )
                        .child(panel),
                )
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
            // The frame-time readout (spec §7.4, `perf::toggle_overlay`),
            // painted above every other shell layer — a diagnostic that
            // must stay visible while the palette/modal/which-key it might
            // be measuring are up. Top-right, clear of the which-key panel
            // (bottom-right) and the status bar. No handlers, no timer:
            // it repaints only when something else invalidates the window,
            // showing values as-of the last invalidation (see
            // `perf_overlay`'s module doc for why that's deliberate).
            .when(self.perf_overlay, |el| {
                el.child(perf_overlay::render(&self.perf, toolbar_height, cx))
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
                shell.dispatch(&ActionId("workspace::switch_2".to_string()), window, cx);
                shell.dispatch(&ActionId("workspace::switch_1".to_string()), window, cx);
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
                    .map(|(ix, ws)| (ix, ws.tree().layout(Rect::UNIT)))
                    .collect()
            });

        let (mut restored, warnings) = session::load(&session_path);
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
                    shell.dispatch(&ActionId(action.to_string()), window, cx);
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
                shell.dispatch(&ActionId("palette::toggle".to_string()), window, cx);
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
                shell.dispatch(&ActionId(action.to_string()), window, cx);
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

    /// Boilerplate shared by the perf tests below: open a window, return
    /// the `VisualTestContext` plus the downcast `ShellView` entity.
    fn open_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::VisualTestContext, gpui::Entity<ShellView>) {
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
        let shell = window.root(&mut cx).unwrap().read_with(&cx, |root, _cx| {
            root.view()
                .clone()
                .downcast::<ShellView>()
                .unwrap_or_else(|_| panic!("root view is not a ShellView"))
        });
        (cx, shell)
    }

    /// Spec §7.4's debug overlay toggle, end to end through the real key
    /// pipeline: `mod+shift+p` (alt is the test/default mod) dispatches
    /// `perf::toggle_overlay`, which paints the readout panel; a second
    /// press removes it. Bounds via the `perf-overlay` debug selector —
    /// the same honest what-the-test-can-see contract as
    /// `empty_workspace_paints_the_hint`.
    #[gpui::test]
    fn perf_overlay_toggles_via_the_bound_action(cx: &mut gpui::TestAppContext) {
        let (mut cx, shell) = open_shell(cx);
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

        let (mut cx, shell) = open_shell(cx);
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
        let (mut cx, shell) = open_shell(cx);
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
                shell.dispatch(&ActionId("perf::reset".to_string()), window, cx);
                assert_eq!(
                    shell.perf.count(),
                    0,
                    "perf::reset should zero the histogram"
                );
            });
        });
    }
}
