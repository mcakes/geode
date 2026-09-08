//! The shell's window root view (spec §3): a single view owning the whole
//! window contents, key dispatch, and workspace state. Chrome (Task 4):
//! `toolbar::toolbar` (the native title bar) on top, `sidebar::sidebar`
//! (workspace indicators + profile icon) on the left, `status::status_bar`
//! (pending keys, reload indicator, theme name) on the bottom. Between
//! them, the tiling tree (Task 3) renders as themed, absolutely-positioned
//! tiles over whatever rect is left. Task 6 wires the real command palette.

pub mod asof_view;
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
pub mod picker;
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
use std::sync::Mutex;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable as _, ScrollHandle,
    UniformListScrollHandle, Window,
};
use gpui_component::input::{InputEvent, InputState};

use crate::actions::ActionRegistry;
use crate::commandline::CommandLine;
use crate::diagnostics::{ActionTail, Diagnostics};
use crate::fontsize::FontSize;
use crate::frame::{Frame, FrameVersions};
use crate::keymap::{Keymap, Matcher, Modifiers};
use crate::log_persist;
use crate::module::{ModuleRoster, TileOccupant};
use crate::palette::PaletteState;
use crate::perf::FrameHistogram;
use crate::reload;
use crate::session;
use crate::theme::ThemeService;
use crate::tiling::{DockSide, Orientation, TileId, Workspaces};
use crate::vimfind::FindStyle;
use geode_core::config::{Config, LayerDoc};
use geode_core::dimensions::DerivedDimensions;
use geode_core::log::{LevelControl, LogLevels, Ring};
use geode_core::query::{DistinctOutcome, QueryKey};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::sync::Arc;

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
    /// The frame's own restored state (Phase 4a §3.6: scope, active slot,
    /// as-of) from `session.toml`'s `[frame]` table, if the file had one
    /// — `ShellView::new` applies it to the just-built frame. `None` for a
    /// fresh session (no file, or one with no `[frame]` table yet) and in
    /// every test setup that doesn't opt in.
    pub restored_frame: Option<crate::session::FrameRecord>,
    /// The `tracing` foundation (Phase 4b Task 2): the ring the
    /// diagnostics tile reads, the control `:level` writes through, and
    /// the levels `[log]` resolved to at startup. `None` in every test
    /// setup that doesn't opt in — logging is then simply not wired up,
    /// never a panic (mirrors `session_path`'s own "missing = skipped").
    pub log: Option<LogServices>,
    /// The last 32 dispatched actions' hashes (Phase 4b Task 6): recorded
    /// by `ShellView::dispatch` before it matches the action, read by the
    /// crash hook (`geode_app::crash::install_panic_hook`) through the
    /// `Arc<Mutex<_>>` handed to it at startup — a shared handle, not a
    /// snapshot, so the hook always sees the latest keypresses right up
    /// to the panic. A `Mutex`, not `RefCell`: this must be `Send + Sync`
    /// to be captured by the 'static panic hook closure alongside
    /// `ActionRegistry::hash_names`'s own `Arc<RwLock<_>>`.
    pub action_tail: Arc<Mutex<ActionTail>>,
}

/// The pieces of the installed `tracing` subscriber the shell needs at
/// runtime: a handle to read the ring (the diagnostics tile), a handle to
/// change the level filter (`:level`), and the levels currently in
/// effect.
pub struct LogServices {
    pub ring: Arc<Ring>,
    pub control: Arc<dyn LevelControl>,
    pub levels: LogLevels,
}

/// What `ShellView` tells the rest of the app about a config reload (§4.5)
/// — plus, since the dimension pickers (Phase 4a §3.3/§3.4), the one thing
/// it needs the app bridge to do FOR it, since `geode-shell` cannot depend
/// on `geode-data` (CLAUDE.md): submit a `Request::Distinct`. The app
/// bridge (`geode-app`, which alone may touch `geode-data`) subscribes to
/// these to know when the views it feeds the data thread need re-sending,
/// when to tell the user a restart is needed, and — for `DistinctRequested`
/// — to call `DataHandle::distinct` and route the outcome back through
/// [`ShellView::deliver_distinct`].
///
/// `PartialEq` only, not `Eq` — `DistinctParams` carries a `Scope`, whose
/// own expression filter can hold a float literal (`Literal::Num(f64)`,
/// `geode_core::scope::expr`) and so stops at `PartialEq` itself; every
/// existing use of this derive (`events.contains(&ShellEvent::
/// ConfigReloaded)`, `shell/tests/reload.rs`) only ever needed `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
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
    /// A dimension picker (`shell::picker`) needs distinct values for one
    /// column, scoped by everything except that column's own selection
    /// (spec §3.4) — the caller has already done that removal. The bridge
    /// calls `handle.distinct(params)`; the result comes back as
    /// `DataEvent::Distinct`, which the bridge routes to
    /// [`ShellView::deliver_distinct`].
    DistinctRequested(geode_core::query::DistinctParams),
}

impl EventEmitter<ShellEvent> for ShellView {}

/// The coalescing key the dimension pickers submit their `Request::
/// Distinct` under (spec §3.4). Reserved, not user-reachable: every real
/// tile's query key comes from `TileId` (spec §2.4), which is a small
/// sequential counter nowhere near `u64::MAX`, so this can never collide
/// with a live tile. `ShellView::deliver` (the `QueryOutcome` route) never
/// sees this key — a picker's own outcome arrives as `DataEvent::Distinct`
/// instead and is routed to [`ShellView::deliver_distinct`], a separate
/// method with its own stale-tag/stale-column guard.
pub const PICKER_KEY: QueryKey = QueryKey(u64::MAX - 1);

/// The coalescing key the diagnostics tile's `Request::Catalog` submits
/// under (Phase 4b §4.5) — same reservation reasoning as [`PICKER_KEY`]
/// just above, one lower so the two can never collide with each other or
/// with a real tile's `TileId`-derived key.
pub const DIAGNOSTICS_KEY: QueryKey = QueryKey(u64::MAX - 2);

/// One column a dimension picker can open (spec §3.3): every categorical
/// column of every dataset, plus every derived dimension. `role` is
/// `"dimension"` for a real `ColumnRole::Dimension` column, `"attribute"`
/// for any other categorical column (an attribute that opted in via
/// `categorical = true`), and `"derived"` for a `[dimensions]` entry —
/// see [`pickable_columns`]. `datasets` lists every dataset the column
/// appears in (first-seen order, appended as more datasets carry it);
/// empty for a derived dimension, which is desk config rather than a real
/// dataset column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickable {
    pub column: String,
    pub role: &'static str,
    pub datasets: Vec<String>,
}

/// Every column a picker can open, in schema order: for each dataset (in
/// the order `SchemaSpec::from_doc` parsed them), every
/// `DatasetSpec::categorical_columns()` entry becomes (or, if a later
/// dataset carries the same column name, extends) a [`Pickable`] —
/// first-seen order, so a column carried by two datasets appears once,
/// where it was first seen, with both datasets listed. Every derived
/// dimension (`[dimensions]`, `DerivedDimensions::all()`) is appended
/// after, role `"derived"`. A key column (`ColumnRole::Key`) is never
/// categorical by default (`SchemaSpec`'s own test,
/// `categorical_defaults_true_for_dimensions_and_false_otherwise...`), so
/// it never reaches `categorical_columns()` and is never pickable.
///
/// Stored on `ShellView.pickable` at construction
/// (`ShellView::new`) and rebuilt by `hot_reload::apply_reload` whenever
/// `datasets` or `dimensions` changes (§4.5) — the same "changed" check
/// `hot_reload::rebuild_slots`'s callers already make for those two docs.
pub fn pickable_columns(config: &Config) -> Vec<Pickable> {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();

    let mut out: Vec<Pickable> = Vec::new();
    for dataset in &schema.datasets {
        for column in dataset.categorical_columns() {
            if let Some(p) = out.iter_mut().find(|p| p.column == column) {
                p.datasets.push(dataset.name.clone());
                continue;
            }
            let role = match dataset.column(column).map(|c| c.role) {
                Some(ColumnRole::Dimension { .. }) => "dimension",
                _ => "attribute",
            };
            out.push(Pickable {
                column: column.to_string(),
                role,
                datasets: vec![dataset.name.clone()],
            });
        }
    }
    for dim in dims.all() {
        out.push(Pickable {
            column: dim.name.clone(),
            role: "derived",
            datasets: Vec::new(),
        });
    }
    out
}

/// Every saved scope, keyed by name (spec §3.9/§3.11) — `defaults::
/// register_scope_actions`'s `scope::<name>` companion to
/// [`pickable_columns`]'s own `register_pick_actions`, and read the same
/// way: `[scopes]` validated against the `datasets`/`dimensions` docs a
/// scope's own columns must resolve against.
///
/// Phase 4b M15: this used to be its own copy of `hot_reload::
/// rebuild_saved_scopes`'s load logic (kept separate because that
/// function is `pub(super)` — internal reload housekeeping, out of
/// `main.rs`'s reach across the crate boundary) — a real duplication,
/// not just a naming difference: the copy here silently dropped
/// `saved_scopes_from_doc`'s diagnostics (`.0` on the tuple) where
/// `rebuild_saved_scopes` prints them. A `pub use` re-export needs no
/// copy: `hot_reload` the *module* stays private, but re-exporting one
/// of its `pub(super)` items under a public name here is exactly as
/// legal as `pickable_columns` living in this module in the first
/// place — nothing about `pub(super)` prevents `shell::mod` itself,
/// which is `hot_reload`'s parent, from naming and re-exporting it.
pub use hot_reload::rebuild_saved_scopes as saved_scopes;

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
    /// The frame's `(scope, grouping, as_of)` versions as of the last
    /// flush (Phase 4a §3.6) — same reasoning as `last_tiles_written`
    /// just above: a frame-only change (no workspace mutation, no tile
    /// state change) must still be noticed by the watcher's tick.
    last_frame_versions_written: (u64, u64, u64),
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
    /// The toolbar's right-aligned filter field (Task 4), now the scope
    /// bar's live text field (Task 4, spec §3.1/§3.11): every keystroke
    /// while it's focused feeds `Frame::set_scope_in_session` through the
    /// `InputEvent::Change` subscription in `new`, coalesced into one
    /// undo entry per focus session. Owned here (rather than built fresh
    /// per render, like `status_bar`/`sidebar`'s stateless element fns) is
    /// required: `Input` is a stateful gpui-component that needs a stable
    /// `Entity<InputState>` across frames to keep its own cursor/selection/
    /// focus state, not something rebuildable from scratch each render.
    filter_input: Entity<InputState>,
    /// The field's value at the moment it took focus (spec §3.11),
    /// captured by the `InputEvent::Focus` arm of `filter_input`'s
    /// subscription and taken by whichever of Enter/Escape/Blur ends the
    /// session first. `Some` only while a text-editing session is open —
    /// `handle_key_down`'s escape branch uses it to restore the pre-focus
    /// text; `Enter`/`Blur` just clear it without restoring anything.
    filter_session_base: Option<String>,
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
    /// The shell-owned diagnostics gatherer (Phase 4b §4.4), created
    /// alongside the frame so every occupant can hold it too. Fed by
    /// the app bridge and by config load/reload (`hot_reload::
    /// apply_reload`); drained by the `cx.observe` set up in `new` for
    /// `:level` persistence, the overlay toggle, and the catalog
    /// request.
    diagnostics: Entity<Diagnostics>,
    /// Set by [`Self::open_module`] right after it splits a fresh tile for
    /// a kind with no existing occupant in the focused workspace (Phase 4b
    /// Task 5): `ensure_occupants` (`shell/occupants.rs`) consumes this —
    /// the ONE new tile it finds with no restored record gets this kind's
    /// factory instead of the roster's default — and clears it, whether or
    /// not a matching factory existed (falling back to the default kind
    /// with a `warn!` when it didn't, same "never a blank, never a panic"
    /// contract `placeholder` upholds for an unknown session kind).
    pending_kind_for_new_tile: Option<String>,
    /// The frame's `(scope, grouping, as_of)` versions as of the last
    /// `on_frame_changed` (Phase 4 §3.10) — compared against the frame's
    /// current ones there to decide whether to open a fresh flip barrier.
    /// Seeded once in `new`, right after a restored session's scope/slot/
    /// as-of are applied, so that restore is never itself mistaken for
    /// "just changed" (which would open a barrier over whatever tiles
    /// happen to exist yet at construction time, before any occupant
    /// does).
    last_flip_versions: FrameVersions,
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
    /// Scratch storage for `visible_tile_keys` (Phase 4 §3.10), same
    /// reasoning as the two fields above — a flip only opens on a user
    /// mutation of scope/grouping/as-of, nowhere near every render, but
    /// there is no reason for it to allocate fresh every time either.
    scratch_visible_keys: Vec<QueryKey>,
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
    /// Every column a dimension picker can open (Phase 4a §3.3),
    /// [`pickable_columns`] over the current config — computed once at
    /// construction and rebuilt by `hot_reload::apply_reload` whenever
    /// `datasets`/`dimensions` changes, the same lifecycle
    /// `sources_baseline`/`datasets_baseline` two fields up describe for a
    /// config-derived cache. `defaults::register_pick_actions` is handed
    /// this same list once at startup (`main.rs`, `test_services`) to
    /// register one `frame::pick_<column>` action per entry — the action
    /// registry itself never changes at runtime (module doc,
    /// `keybindings_view`), so a column a later reload adds has no
    /// palette-reachable action of its own; picking it still works via
    /// `frame::pick`'s two-stage flow.
    pickable: Vec<Pickable>,
    /// The open dimension picker's own pure state (Phase 4a §3.3), or
    /// `None` when closed/never opened — the `keybindings`/`settings`
    /// fields' own contract. Set fresh by [`picker::open`] each time and
    /// cleared by [`close_modal`](Self::close_modal), same as the other
    /// two dialogs.
    picker: Option<picker::PickerState>,
    /// The tag [`picker::open`]/[`picker::request_values`] hands out next
    /// (Phase 4b M5) — monotonic across the whole session, never reset
    /// per open. `PickerState::new` used to always start a fresh picker
    /// at `tag: 0`, so two separate opens on the same column produced
    /// the *same* sequence of tags (0, then 1 once the first request
    /// went out); a `DistinctOutcome` that arrived late from the first
    /// open could then be mistaken for the second open's own answer.
    /// Reusing one counter across opens instead of restarting it makes
    /// every tag this session ever hands out unique.
    next_picker_tag: u64,
    /// Scroll state for the picker's `Values`-stage `uniform_list` (fix
    /// round 1, Finding 1) — the `keybindings_scroll`/`settings_scroll`/
    /// `palette_scroll` split, one gpui type over: `uniform_list` is
    /// self-virtualizing (it never lays out an off-screen row), but that
    /// buys nothing for scroll-FOLLOW — nothing scrolls the viewport when
    /// `selected` moves without this handle, so keyboard navigation past
    /// [`palette::VISIBLE_ROWS`] would leave the highlight off-screen with
    /// only its index having changed. `gpui::UniformListScrollHandle`, not
    /// the plain `gpui::ScrollHandle` the other three use: `uniform_list`
    /// only tracks scroll through its own handle type (see
    /// `picker::sync_picker_scroll`'s doc comment for the call sites that
    /// drive it).
    picker_scroll: UniformListScrollHandle,
    /// The open as-of dialog's own pure state (Phase 4a §3.6), or `None`
    /// when closed/never opened — the `picker`/`keybindings`/`settings`
    /// fields' own contract. Set fresh by [`asof_view::open`] each time
    /// and cleared by [`close_modal`](Self::close_modal), same as the
    /// other three dialogs.
    as_of_dialog: Option<asof_view::AsOfState>,
    /// Today's local date (Phase 4b Task 1 fix round 1, MIN-9) —
    /// refreshed once per reload-poll tick (~500ms, alongside the flip
    /// sweep and the dirty-session flush) rather than read fresh on
    /// every paint. Before this, `render`'s own `chrono::Local::now()`
    /// call (feeding `Frame::bar_model`'s `(versions, today)` cache key,
    /// M12) ran on every single render — including every one of the
    /// ~100% of frames that hit the cache — new per-frame clock-read
    /// work on the render path for a value that only meaningfully
    /// changes once a day.
    pub(super) today: chrono::NaiveDate,
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

        // The scope bar's live text field (Task 4, spec §3.1/§3.8/§3.11):
        // one subscription for the life of the window, same lifecycle
        // shape as `palette_input`'s below. `Focus` opens a text-editing
        // session (`begin_scope_session`) and remembers the pre-focus
        // value for Escape to restore; `Change` feeds every keystroke
        // into the session, coalescing into one undo entry; `PressEnter`
        // and `Blur` both close the session (`end_scope_session`) — Enter
        // additionally hands focus back to the shell root, `Blur` doesn't
        // need to (something else already has it). Escape's own restore
        // is handled in `handle_key_down`'s filter-focused branch, ahead
        // of this subscription ever seeing the resulting `Blur`.
        cx.subscribe_in(
            &filter_input,
            window,
            |view, input, event, window, cx| match event {
                InputEvent::Focus => {
                    view.filter_session_base = Some(input.read(cx).value().to_string());
                    view.frame.update(cx, |f, _| f.begin_scope_session());
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.frame.update(cx, |f, cx| {
                        let mut s = f.scope().clone();
                        s.text = (!text.trim().is_empty()).then_some(text);
                        if f.set_scope_in_session(s) {
                            cx.notify();
                        }
                    });
                }
                InputEvent::PressEnter { .. } => {
                    view.filter_session_base = None;
                    view.frame.update(cx, |f, _| f.end_scope_session());
                    view.focus_handle.focus(window, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.filter_session_base = None;
                    view.frame.update(cx, |f, _| f.end_scope_session());
                }
            },
        )
        .detach();

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
            // clears all four fields, so at most one is `Some` here — the
            // routing cannot land in a stale state left over from an
            // earlier open.
            if let Some(state) = view.keybindings.as_mut() {
                state.set_query(query);
                view.keybindings_scroll.scroll_to_item(0);
            } else if let Some(state) = view.settings.as_mut() {
                state.set_query(query);
                view.settings_scroll.scroll_to_item(0);
            } else if let Some(state) = view.picker.as_mut() {
                // No `set_query` method (unlike the two dialogs above) —
                // `PickerState` has no other side effect to bundle with a
                // query edit, so the two-line reset lives here rather than
                // behind a one-line wrapper with a single caller.
                state.query = query;
                state.selected = 0;
                picker::sync_picker_scroll(view);
            } else if let Some(state) = view.as_of_dialog.as_mut() {
                // Unlike the three dialogs above, this field's raw text IS
                // the value being edited (spec §3.6), not a filter over
                // something else — re-resolve it and store the outcome
                // (`resolved`/`error`) for `build` to show; see
                // `asof_view::on_query_changed`'s own doc comment.
                asof_view::on_query_changed(state, &query, chrono::Utc::now());
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

                // Sweep the flip barrier's deadline (Phase 4b M7),
                // unconditionally on every tick just like the session
                // flush right below — one always-running timer rather
                // than a fresh detached one per scope/grouping/as-of
                // mutation (`on_frame_changed` used to spawn one on every
                // such change; a burst of keystrokes spawned a burst of
                // timers, all racing to sweep the same barrier). `sweep`
                // itself is a cheap no-op once nothing is open or the
                // deadline hasn't passed, so this costs nothing on a
                // quiet tick. The tradeoff (spec §3.10's as-built note,
                // corrected in Task 1 fix round 1 MIN-5): a barrier now
                // releases on the poll loop's next iteration after
                // `FLIP_DEADLINE`, not exactly at it — bounded by that
                // whole iteration (this timer, then the session flush
                // below, then the `reload::scan` further down), not by
                // the timer interval alone, since nothing sweeps again
                // until the loop comes back around to this line.
                let Ok(frame) = this.update(cx, |view, _cx| view.frame.clone()) else {
                    return; // window/entity gone; stop polling
                };
                frame.update(cx, |f, cx| {
                    if f.sweep(Instant::now()) {
                        cx.notify();
                    }
                });

                // Copy the frame-time histogram into `Diagnostics`
                // (Phase 4b open question 2's ruling), same tick — a
                // no-op, allocation-free, unless a diagnostics tile is
                // actually watching. `refresh_frame_hist` itself also
                // compares before copying (Task 4 fix round 1, MAJ-3),
                // but the `~176`-byte `view.perf.clone()` (`FrameHistogram`
                // is `[u32; 36]` plus four scalars) that used to happen
                // unconditionally right here, every ~500ms tick,
                // regardless of `watchers()`, is gated on it too now
                // (Task 4 fix round 1, MIN-1 — the comment used to claim
                // this whole thing was already "allocation-free unless
                // watching" while the clone ran every tick regardless;
                // now it's actually true, not just documented that way).
                let Ok((diagnostics, watched)) = this.update(cx, |view, cx| {
                    let watched = view.diagnostics.read(cx).watchers() > 0;
                    (view.diagnostics.clone(), watched)
                }) else {
                    return; // window/entity gone; stop polling
                };
                if watched {
                    let Ok(perf) = this.update(cx, |view, _cx| view.perf.clone()) else {
                        return; // window/entity gone; stop polling
                    };
                    diagnostics.update(cx, |d, cx| {
                        if d.refresh_frame_hist(&perf) {
                            cx.notify();
                        }
                    });
                }

                // Refresh `today` (Phase 4b Task 1 fix round 1, MIN-9),
                // same tick, same "cheap no-op unless it actually
                // changed" shape as the sweep just above — this is the
                // one clock read the whole ~500ms tick needs; `render`
                // (and therefore `Frame::bar_model`'s cache key) reads
                // `self.today` rather than calling `chrono::Local::now()`
                // itself, so a held key no longer pays a clock read on
                // every repaint for a value that only changes once a
                // day. Only notifies when the date actually moved on —
                // any other trigger repaints "for free" with the fresh
                // value already in place.
                let Ok(changed) = this.update(cx, |view, _cx| {
                    let today = chrono::Local::now().date_naive();
                    let changed = view.today != today;
                    view.today = today;
                    changed
                }) else {
                    return; // window/entity gone; stop polling
                };
                if changed {
                    let _ = this.update(cx, |_view, cx| cx.notify());
                }

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
                            tracing::warn!(target: "geode::session", "failed to save session: {e}")
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

        // The shared frame (§4): built from whatever `[groupings]`/
        // `[scopes]` (plus the `datasets`/`dimensions` docs they validate
        // against) config resolved to — see `hot_reload::rebuild_slots`/
        // `rebuild_saved_scopes`, shared with `apply_reload`'s own
        // rebuilds.
        let frame = {
            let slots = hot_reload::rebuild_slots(&services.config);
            // `true` (Phase 4b Task 1 fix round 1, MIN-8): the frame's
            // own initial load is the one startup caller that reports —
            // `main.rs`'s `saved_scopes(&config)` call (action
            // registration, before this even runs) passes `false`, so a
            // malformed `scopes.toml` entry doesn't print twice.
            let saved = hot_reload::rebuild_saved_scopes(&services.config, true);
            cx.new(|_| Frame::new(slots, saved, user_dir.clone()))
        };
        // A slot or scope saved by a module (`:group save N`, `:scope
        // save NAME`) is drained and persisted here — see
        // `on_frame_changed`'s own doc comment (§4.2/§3.9: the frame is
        // pure and has no file access, so `ShellView` is the one place
        // that can do the write). `observe_in` (not `observe`) because
        // Task 4's text-field reflection needs `&mut Window` to call
        // `InputState::set_value`.
        //
        // Registered here, before any tile occupant exists (occupants
        // are built later, as tiles are hosted), this is the FIRST
        // observer of `frame`'s notify — gpui fans a notify out to an
        // entity's observers in registration order. `on_frame_changed`'s
        // `open_flip` branch below relies on that: every occupant sees
        // the barrier already open (or the flip barrier's key set
        // finalised) before its own `on_frame_changed` runs in the same
        // flush, which is what lets a non-following tile
        // (`BlotterTile::on_frame_changed`'s `barrier_wants`/`arrived`
        // branch) self-arrive without ever requerying.
        cx.observe_in(&frame, window, |view, frame, window, cx| {
            view.on_frame_changed(frame, window, cx)
        })
        .detach();

        // The shell-owned diagnostics gatherer (Phase 4b §4.4), created
        // alongside the frame — see the field's own doc comment. Seeded
        // from `services.log`'s levels when logging is wired up (`None`
        // in every test setup that doesn't opt in, mirroring `log`
        // itself), `LogLevels::default()` otherwise.
        let diagnostics = {
            let levels = services
                .log
                .as_ref()
                .map(|l| l.levels.clone())
                .unwrap_or_default();
            cx.new(|_| Diagnostics::new(levels))
        };
        // The config this window started with already carries whatever
        // `Config::load` diagnosed — `main.rs`'s own startup
        // `print_diagnostic` loop logs the same list to `geode::config`;
        // recorded here too so the diagnostics tile's "config" section
        // has it from the very first frame, not only from the first live
        // reload (`apply_reload`'s own `note_config` call, `hot_reload.rs`).
        diagnostics.update(cx, |d, _cx| {
            d.note_config(
                services.config.diagnostics.clone(),
                std::time::SystemTime::now(),
            );
        });
        // Same drain-only shape as the frame's own observer above, minus
        // the `Window` — none of `on_diagnostics_changed`'s three drains
        // need one.
        cx.observe(&diagnostics, |view, diagnostics, cx| {
            view.on_diagnostics_changed(diagnostics, cx);
        })
        .detach();

        // Restore a saved session's scope/slot/as-of (Task 3, spec §3.6
        // "state-as-config"), applied directly to the just-built frame
        // rather than threaded through `Frame::new` — `main.rs` restores
        // the workspace layout the exact same way, after `services` is
        // built. `clear_history` afterwards drops the undo entry
        // `set_scope` just pushed: a restored session must not start with
        // a phantom "undo" back to the empty scope nobody actually chose.
        if let Some(record) = services.restored_frame.clone() {
            frame.update(cx, |f, _cx| {
                f.set_scope(record.scope);
                f.set_active_slot(record.active_slot);
                f.set_as_of(record.as_of);
                f.clear_history();
            });
        }
        // Seeded from the just-built frame (see the field's own doc
        // comment) so a restored session's scope/slot/as-of is never
        // itself read as "just changed" by the first real
        // `on_frame_changed`.
        let last_flip_versions = frame.read(cx).versions();

        // M8: the docs the data engine actually starts with — see
        // `sources_baseline`'s field doc.
        let sources_baseline = services.config.layered_docs("sources").to_vec();
        let datasets_baseline = services.config.layered_docs("datasets").to_vec();
        // The dimension pickers' column list (Phase 4a §3.3) — see
        // `pickable`'s field doc.
        let pickable = pickable_columns(&services.config);

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
            last_frame_versions_written: (0, 0, 0),
            pending_focus_restore: false,
            divider_drag: None,
            tile_drag: None,
            filter_input,
            filter_session_base: None,
            perf: FrameHistogram::new(),
            last_render_started: None,
            perf_overlay: false,
            frame,
            diagnostics,
            pending_kind_for_new_tile: None,
            last_flip_versions,
            occupants: HashMap::new(),
            visible_tiles: HashSet::new(),
            scratch_all_tiles: HashSet::new(),
            scratch_active_tiles: HashSet::new(),
            scratch_visible_keys: Vec::new(),
            restart_required: None,
            sources_baseline,
            datasets_baseline,
            pickable,
            picker: None,
            next_picker_tag: 0,
            picker_scroll: UniformListScrollHandle::new(),
            as_of_dialog: None,
            today: chrono::Local::now().date_naive(),
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
    /// Also clears all four dialogs' state (`keybindings`, `settings`,
    /// the dimension picker — Phase 4a §3.3 — `picker`, and the as-of
    /// dialog — Phase 4a §3.6 — `as_of_dialog`). That is not tidiness:
    /// the shared `dialog_input` subscription routes by "whichever state
    /// is `Some`", so a stale `settings` left behind by an earlier open
    /// would swallow the *keybinding* dialog's queries.
    pub(crate) fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        self.settings = None;
        self.keybindings = None;
        self.picker = None;
        self.as_of_dialog = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Fired by the `cx.observe_in(&frame, ..)` set up in `new` whenever
    /// the frame notifies — which covers both the keyboard's
    /// `frame::slot_*` dispatches and a module's own `:group save N`. A
    /// slot saved by a module is persisted here, off the UI thread,
    /// because the frame is pure and the module has no file access
    /// (§4.2): the frame only remembers the save in `pending_persist`,
    /// and this is where it gets drained and actually written.
    fn on_frame_changed(
        &mut self,
        frame: Entity<Frame>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Phase 4 §3.10: a scope/grouping/as-of change opens a flip
        // barrier over every currently visible tile before anything
        // requeries, so the tiles that follow it all swap to the new
        // triple in one notify pass rather than painting one at a time
        // as their own outcomes happen to land. `data`/`config` bumps
        // never open one — every tile already requeries independently
        // for those (§4.1), and there is no "everyone at once" to
        // coordinate. `open_flip` itself bumps no version, so the notify
        // it does not emit cannot re-enter this branch.
        //
        // "before anything requeries" holds because this observer is
        // registered (in `new`, above) before any tile occupant's own —
        // gpui calls one entity's observers in registration order, so
        // `open_flip` below always finishes before a single tile's own
        // `on_frame_changed` runs for the same notify. A non-following
        // tile's self-arrival from its own `on_frame_changed`
        // (`BlotterTile`'s `barrier_wants`/`arrived` branch) depends on
        // the barrier already being open with the full key set by the
        // time it checks — it never requeries, so nothing else would
        // open one for it.
        let now_v = frame.read(cx).versions();
        let last = self.last_flip_versions;
        if now_v.scope != last.scope || now_v.grouping != last.grouping || now_v.as_of != last.as_of
        {
            self.last_flip_versions = now_v;
            let mut keys = std::mem::take(&mut self.scratch_visible_keys);
            self.visible_tile_keys(&mut keys);
            frame.update(cx, |f, _| f.open_flip(keys.iter().copied(), Instant::now()));
            self.scratch_visible_keys = keys;
            // Phase 4b M7: no detached per-mutation timer here any more —
            // a burst of keystrokes used to spawn one `FLIP_DEADLINE`
            // timer each, all racing to sweep the same barrier. The
            // reload-poll loop (`ShellView::new`, ~500ms) sweeps every
            // tick instead, so the deadline is "released on the next
            // tick after `FLIP_DEADLINE`" rather than exactly on it —
            // see that loop's own comment and spec §3.10's as-built note.
        }
        if let Some((slot, grouping)) = frame.update(cx, |f, _| f.take_pending_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = crate::frame::persist_slot_to_user_config(&dir, slot, &grouping)
                    {
                        tracing::warn!(target: "geode::config", "{e}");
                    }
                })
                .detach();
        }
        // A scope saved by a module (`:scope save NAME`, spec §3.9) is
        // persisted here too — same reasoning as the grouping slot above:
        // the frame is pure and has no file access.
        if let Some((name, scope)) = frame.update(cx, |f, _| f.take_pending_scope_persist())
            && let Some(dir) = self.user_dir.clone()
        {
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = crate::frame::persist_scope_to_user_config(&dir, &name, &scope)
                    {
                        tracing::warn!(target: "geode::config", "{e}");
                    }
                })
                .detach();
        }
        // Reflect the frame's text back into the field (Task 4, spec
        // §3.11): an unfocused field always shows the frame's truth — a
        // scope set elsewhere (a saved-scope load, a module's own
        // `:scope` command) must show up here even though this field
        // never had focus. Skipped while the field IS focused: the user's
        // own typing is the truth then, and `set_value` would stomp the
        // caret/selection mid-edit. Reading the value is a `SharedString`
        // clone per frame notify, not per render — fine.
        if !self
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            let frame_text = frame.read(cx).scope().text.clone().unwrap_or_default();
            let field_text = self.filter_input.read(cx).value().to_string();
            if field_text != frame_text {
                self.filter_input.update(cx, |i, cx| {
                    i.set_value(frame_text, window, cx);
                });
            }
        }
        cx.notify();
    }

    /// Fired by the `cx.observe(&diagnostics, ..)` set up in `new`
    /// whenever the entity notifies: drains the two pending requests a
    /// module can queue but never reach `ShellView` to act on directly
    /// (spec ruling — modules never reach `ShellView`) — `request_level`'s
    /// runtime apply + persist, and `request_overlay_toggle`. The catalog
    /// request drain lives in the app bridge (`geode-app` is the only
    /// crate allowed to touch `geode-data`), not here.
    fn on_diagnostics_changed(&mut self, diagnostics: Entity<Diagnostics>, cx: &mut Context<Self>) {
        let (pending_level, pending_overlay) = diagnostics.update(cx, |d, _cx| {
            (d.take_pending_level(), d.take_pending_overlay_toggle())
        });
        if let Some((target, level)) = pending_level {
            let levels = diagnostics.read(cx).levels.clone();
            if let Some(log) = &self.services.log
                && let Err(e) = log.control.set(&levels)
            {
                tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
            }
            if let Some(dir) = self.user_dir.clone() {
                cx.background_executor()
                    .spawn(async move {
                        if let Err(e) =
                            log_persist::persist_log_level_to_user_config(&dir, &target, level)
                        {
                            tracing::warn!(target: "geode::config", "failed to persist [log]: {e}");
                        }
                    })
                    .detach();
            }
        }
        if pending_overlay {
            self.perf_overlay = !self.perf_overlay;
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

    /// The shell-owned diagnostics entity every occupant can hold too
    /// (Phase 4b §4.4).
    pub fn diagnostics(&self) -> &Entity<Diagnostics> {
        &self.diagnostics
    }

    /// Open a module tile of `kind` in the focused workspace (Phase 4b
    /// Task 5, spec ruling: "`diagnostics::open` opens by kind through the
    /// shell, not through the module"): focus an existing occupant of that
    /// kind wherever it lives (the main tree or a dock) if one exists,
    /// else split the focused tile (the same path `ctrl+v`/`workspace::
    /// split_right` takes — `Tree::split` always focuses the new tile) and
    /// set [`Self::pending_kind_for_new_tile`], which `ensure_occupants`
    /// (`shell/occupants.rs`) consumes on its very next call — the same
    /// render pass, since `ensure_occupants` runs at the top of every
    /// `render` and this always `cx.notify()`s.
    pub fn open_module(&mut self, kind: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.services.workspaces.active();
        let found: Option<(TileId, Option<DockSide>)> = ws
            .tree()
            .tiles()
            .into_iter()
            .find(|id| self.occupant_kind(*id) == Some(kind))
            .map(|id| (id, None))
            .or_else(|| {
                ws.docks().iter().find_map(|(side, dock)| {
                    dock.tree()
                        .tiles()
                        .into_iter()
                        .find(|id| self.occupant_kind(*id) == Some(kind))
                        .map(|id| (id, Some(side)))
                })
            });
        if let Some((tile, side)) = found {
            let ws = self.services.workspaces.active_mut();
            match side {
                Some(side) => {
                    ws.focus_dock_tile(side, tile);
                }
                None => {
                    ws.focus_main_tile(tile);
                }
            }
            self.session_dirty = true;
            cx.notify();
            return;
        }
        // MIN-7 (Phase 4b Task 5 fix round 1): two `open_module` calls for
        // the same kind within one render (a double `mod+shift+d` press,
        // key-repeat) would otherwise both miss the "existing occupant"
        // search above — the first call's split tile has no occupant yet
        // (`ensure_occupants` only creates one at the top of the *next*
        // render), so the second call splits again and overwrites
        // `pending_kind_for_new_tile`, leaving one tile hosting `kind` and
        // a stray second one hosting the default kind. A pending request
        // for the SAME kind is a no-op — the tile that request will
        // create is, for all `open_module`'s purposes, already "the one
        // open occupant of this kind" the moment it's requested, whether
        // or not `ensure_occupants` has caught up yet. A pending request
        // for a *different* kind still overwrites, same as before (last
        // request wins, unambiguous — nothing between two `open_module`
        // calls for different kinds within one render should silently
        // drop either).
        if self.pending_kind_for_new_tile.as_deref() == Some(kind) {
            return;
        }
        self.services
            .workspaces
            .split_active(Orientation::Horizontal);
        self.pending_kind_for_new_tile = Some(kind.to_string());
        self.session_dirty = true;
        cx.notify();
    }

    /// The open dimension picker's state, if any (Phase 4a §3.3/§3.4) —
    /// cross-crate test reach only, the same door `module::recording`
    /// opens for `geode-blotter`'s tests: `geode-app`'s bridge tests need
    /// to see a picker's `values` land (or fail to) without a `dispatch`
    /// call of their own to drive from (`dispatch` is `pub(super)`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn picker(&self) -> Option<&picker::PickerState> {
        self.picker.as_ref()
    }

    /// Deliver a `DataEvent::Distinct` outcome (spec §3.4), routed here by
    /// the app bridge from the `ShellEvent::DistinctRequested` it submitted
    /// on this same picker's behalf. Dropped — no picker mutation, no
    /// notify — unless every one of these holds: a picker is open, its
    /// stage is `Values` (a `Columns`-stage picker asked for nothing and
    /// wants nothing), the outcome names that stage's own column (a picker
    /// that moved on to a different column between request and reply), and
    /// the outcome's tag matches the picker's *latest* `request_values`
    /// call (`PickerState::tag`, bumped once per request) — an outcome
    /// racing in from a superseded request (the user re-opened the same
    /// column, or the query pool simply finished them out of order) is
    /// exactly the stale result §7.3 says must never be rendered.
    pub fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        let Some(state) = self.picker.as_mut() else {
            return;
        };
        let picker::Stage::Values { column } = &state.stage else {
            return;
        };
        if *column != outcome.column || outcome.tag != state.tag {
            return;
        }
        state.values = Some(outcome.values);
        cx.notify();
    }
}

#[cfg(test)]
mod tests;
