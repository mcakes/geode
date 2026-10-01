# geode-shell

The Geode shell: the tiling window manager, workspaces and docks, the
keymap engine, the command palette, the shared frame (scope, grouping,
as-of), theming, the modal dialogs, and the contract a module implements
to live in a tile. Built on gpui and gpui-component.

This crate never depends on `geode-data` or on any module crate. A module
gets shell-side handles only (a `TileId`, a `FrameRef`, the action
registry); anything it needs from data it asks `geode-data` for itself,
and the two meet only in `geode-app`. A tile's `FrameRef` is bound to its
workspace for life; reads resolve to that workspace's lane (see
[the shared frame](../../docs/current/shell.md#the-shared-frame)).

Current behavior and rationale: [`docs/current/shell.md`](../../docs/current/shell.md).
Keyboard ownership, palette, completion, choices, and frame-picker contracts:
[input and dialogs](../../docs/current/input-and-dialogs.md).

## Layout

State and configuration helpers are separated from window integration.
Many helpers have pure models tested without a window; some also publish
GPUI globals or provide rendering helpers.

**State, configuration, and shared helpers**

| Module | Holds |
|---|---|
| `tiling` | Split/leaf/stack trees, workspaces, docks, divider and drop-zone geometry. Rendering and navigation share tree geometry; see [tiling contracts](../../docs/current/tiling.md) for focus, transfer, resize, and restoration rules. |
| `keymap` | Keystrokes, predicates, layered binding resolution, sequences, counts, and module fragments. `GRID`/`TILELIST` are the flags a tile publishes for the shared motions. See [keymaps and actions](../../docs/current/keymaps.md) for matching, filtering, and editor limitations. |
| `actions` | The shared action registry; the keymap maps keys to action ids and the palette lists them. |
| `frame` | The shared frame: scope with undo/redo, the active grouping slot, as-of, recent publishes, saved scopes, named expressions (`expressions.toml`, rebuilt on reload), and the data and config generations, as one value every tile observes. The selection lives in lanes — one shared, one per pinned workspace (`pin`, `unpin`) — read through `FrameView` and written through `FrameViewMut`; every lane's generations come from one counter, so a number names one value in any lane. See [workspace lanes](../../docs/current/shell.md#workspace-lanes). `effective_scope` composes the frame and tile scope layers and resolves named references, returning the first missing or invalid one as an error instead of a scope. |
| `frame_ref` | `FrameRef`, the workspace-bound frame handle a module receives: `read`/`update` resolve to that workspace's lane. |
| `scopebar`, `commandline`, `palette_usage`, `listfilter`, `choice`, `vimnav`, `vimfind`, `footer` | The models behind the scope bar, the per-tile `:` line, palette ranking (frecency), filtered lists, choice-with-typeahead, vim-style list motion and `/` find, and dialog footer hints. |
| `exprcomplete` | Pure scope-expression completion: derives context from text and caret, retains up to 50 ranked rows, and caches categorical values per column. Loading tags reject stale replies; superseded loading entries are discarded so revisiting a column can request again. Scope changes clear the cache. Whole/Add frame dialogs also offer named expressions, whose acceptance stages a name and erases the typed prefix. |
| `dialogmode` | Shared Normal/Filter transitions. Keyboard and frozen-row clicks snapshot the query; Escape restores it and bare Enter keeps it, with neither exit acting on a row. See [dialog filtering](../../docs/current/shell.md#dialog-filtering) for focus and stage-specific exceptions. |
| `theme`, `fonts`, `fontsize`, `linenumbers`, `tileadd`, `tips` | Settings and their pure resolution rules: the bundled gpui-component themes, the bundled Inter and JetBrains Mono faces, the rem scale knob, `[ui] line_numbers`, the add-tile direction, tooltip text from the live keymap. |
| `config_write`, `keymap_edit`, `log_persist`, `reload` | Ordered user-layer writes, comment-preserving keyed edits, log-level persistence, and reload detection/rejection. See the [configuration contract](../../docs/current/configuration.md#runtime-edits) for write guarantees and the limits of keep-last-good. |
| `session` | Session encoding, local recovery, and atomic file replacement. Shell integration owns periodic snapshots and shutdown saves. See [session persistence](../../docs/current/shell.md#session-format) for recovery boundaries, unavailable modules, and write-failure behavior. |
| `diagnostics` | Source health, generations, independent config/data diagnostics, stopped data threads (`StoppedThread`, `thread_label`) and the prepared `StoppedSegment`, the `Busy`-refusal total, section versions, cached status summary, and watched/explicit catalog demand. See the [diagnostics contract](../../docs/current/shell.md#diagnostics-state-and-demand). |
| `colfit` | The pure column-fit measure behind `:autosize` and `tile::autosize_columns`: `FitMetrics` (mono advance at `text_sm`, the `XSmall` cell padding and cursor border at the window's rem, clamped to 2.5–40 rem), the `FittedWidths` map by stable column key, and its lenient `column_widths` session read/write, which clamps a restored width to 25–560 px. `NO_TABLE` and `NOTHING_TO_FIT` are the two refusals. See [autosized columns](../../docs/current/features.md#autosized-columns). |
| `perf` | The always-compiled frame-time histogram. |
| `menu` | The action menu model and renderer: `Row` (`Action`/`Separator`/`Section`), `ActionRow` (a key hint resolved through the live keymap, `enabled` with its reason), `Menu` (`step` lands only on enabled actions; `pick` answers a disabled row's reason), `render_menu` over a `MenuHost` (a press outside runs the caller's close). The shell's row menu and every tile's `.` menu use it; `geode-tile` re-exports it. |
| `popover` | Popup geometry, the popover `surface`, `anchor_popup` and the `row_shell`/`empty_row` row frames; `geode-tile` re-exports it. |
| `dimension` | The row menu's plugin seam: `DimensionAction` (`id`, `title`, the `column` whose section it sits in, `available` — enabled, or disabled with the reason its row shows — and `run` against an `ActionCx` once the menu has closed), `RowPick` (`Open { kind }` or `Action { index }`), and `menu_rows`: one section `{column} · {value}` per context column that has rows, the clicked column (`first`) leading, each "Open {Kind}" in the first section whose column that kind accepts, then that column's actions. Actions are Rust impls registered with `ModuleRoster::add_action` in `geode-app`. `UrlOpener` is a gpui global wrapping an `OpenUrl` function: when set, `ActionCx::open_url` calls it instead of the OS (tests record URLs through it). |
| `defaults` | The builtin action set and keymap, the Builtin config layer; `MOTION_ACTIONS` (the shared `motion::*` vocabulary, category "Motion", handled by no shell code so it falls through to the focused tile) and `GRID_MOTION_CONTEXT`, the one context the grid motions ship under; `shared_motion_context` names where the keybindings dialog writes a Motion row's edits, after clearing the action's user overrides. |

**Window integration**

| Module | Holds |
|---|---|
| `shell` | `ShellView`, the window owner: tile occupants and focus, input dispatch, rendering, drag and drop, chrome (the status bar's first left segment is the stopped data-thread segment), dialogs and palette, hot reload, and session I/O. `aggregates` renders selection extents and totals prepared by grid tiles. Shared color helpers and `kbd` keep chrome presentation consistent. Tests live in `shell/tests/`. |
| `shell/pin` | The workspace pin toggle (`frame::pin_workspace` and the toolbar glyph), the workspace-switch hook that re-seeds the flip baseline, and the scope field's rebinding to the active lane. |
| `shell/dialog` | Modal stack ownership, opening, shared-input synchronization, and focus restoration. Only the top dialog renders and receives keys. A kind cannot open twice; pop restores the covered dialog's text and caret. See [modal lifetime](../../docs/current/input-and-dialogs.md#modal-lifetime-and-focus). |
| `shell/choicedialog` | The filter-only choice modal behind the grouping picker, the scope picker (`frame::scope`: the frame's live saved scopes, loaded through `ShellView::load_saved_scope`), the tile-kind and `tile::open_with` pickers, the column lists, and `Set log level…`. One `Target` per use decides rows, title, footer and commit. See [choices](../../docs/current/input-and-dialogs.md#grouping-scope-tile-log-and-column-choices). |
| `shell/scope_expr_view` | Frame expression editing in Whole, Term, and Add modes. Whole/Add stage named-expression chips; `mod+s` saves typed text as a named definition. Term mode can replace one guarded term with a named reference. |
| `shell/expr_suggest` | Shared expression-completion controller and renderer for frame, Scopes, and Expressions fields. Observes text and caret changes, requests values under reserved `EXPR_KEY`, and accepts rows through undoable range replacement. Tab accepts; Shift-Tab, Up/Down, and Ctrl-P/Ctrl-N move the highlight. Named rows stage references only in frame Whole/Add mode. Named-definition fields request unscoped values; unresolved scope references report an error before requesting. |
| `module` | The module-hosting contract: `TileContent` (including `closed`, called once on removal after `set_visible(false)`, `dimension_context`, `press_context` (the row a right press landed on, for the row menu; `None` by default), `tile_columns`, `launched`, and `autosize_columns`, whose default refuses with `colfit::NO_TABLE`; `tile::autosize_columns` calls it on the focused occupant only and shows a refusal as a notice), `ModuleFactory` (including `accepts` and `launch_state`), `ModuleRoster` (with `add_action`/`actions` for the row menu's `DimensionAction`s, and `context_columns`: every accepted column, then every action's column), `Delivery`, `StackHandle`; and the page seam beside it: `PageContent`, `PageFactory` (with `toggle_binding`), `PageOccupant`, `PageRoster` (whose `keymap_fragments` also emits the shell-generated toggle doc), and the `ShellActions` handle. `module::recording` is the test double a downstream crate hosts a neighbour or a page with. See [pages](../../docs/current/shell.md#pages). |
| `shell/row_menu` | The row menu overlay (`RowMenu`): `tile::context_menu` opens it on the focused tile's `dimension_context` at the recorded `anchor` (or the tile's top-left); a right press opens it on the occupant's `press_context` at the pointer. Its keys are `j`/`down`, `k`/`up`, `enter` and `escape`; other bare keys are consumed and a chord closes it and dispatches. A pick, `escape`, a press outside, any dispatch, a dialog or the palette closes it; a disabled row is inert. `ActionCx` gives an action `open_tile` (a split, as `g m`), `open_url` (the OS's `App::open_url`, or a set `dimension::UrlOpener`) and `notice` (any text, so an action can report what it did). A row with nothing to offer shows the notice `NO_ROW_ACTIONS` ("no actions for this row") instead. See [the row menu](../../docs/current/shell.md#row-menu). |
| `shell/page` | One page at a time in place of the tile surface (the toolbar and status bar stay): `open_page` (create on first open, then show and focus; a different kind replaces the retained page after stashing its state), `close_page`, `toggle_page`, `focus_home`, and the deferred `shell_actions` handle a page dispatches registered actions through. |
| `shell/objectdialog` | Domain drafts, staged editing, validation, overrides, and debounced persistence. `render::open_object` opens a named object's edit stage or reports that it is undefined; `render::open_column` opens a tile's view (or the column's owning dataset) on one column's Column stage, reporting each failure in the footer. `apply::queue_object` queues a whole user-layer definition with pending edits. The Expressions adapter validates named definitions and identifies referring scopes before deletion. See [configuration dialogs](../../docs/current/configuration-dialogs.md) for ownership and failure boundaries. |

## Globals

The workspace has four module-visible GPUI globals, written by the shell:
`linenumbers::UiSettings`, `tips::Chords`, `clock::AppClock`, and
`series::SeriesSettings`. Add another only for state that is genuinely app
wide and module visible.

## Features

- `test-support` exposes `module::recording` and a few accessors outside
  `#[cfg(test)]`, so the module crates' tests can host a recorded
  neighbour. CI checks `cargo check -p geode-shell --features test-support
  --all-targets` to cover the public hosting-test configuration.
- `profiling` turns on gpui's own profiler (frame and input-latency
  histograms, the debug overlay, hang detection) and the two shell actions
  that surface it. Enabled through `geode-app`'s same-named feature.

## Commands

```sh
cargo test -p geode-shell
cargo bench -p geode-shell     # the pure shell cores
```

Test fixtures live in `src/shell/tests/mod.rs`. They bind `ctrl+v` and
`ctrl+h` in a test layer to create tiles (the shipped keymap has no such
chord) and carry a `rec` recording factory in the roster.

## Rules this crate pins

The current contracts and their reasons are in
[`docs/current/shell.md`](../../docs/current/shell.md). The ones a first
change most often hits:

- Every action must be keyboard-reachable, and nothing may stall the
  render thread. Per-frame heap churn is a defect.
- Dialogs open through `shell::dialog::open_shell_dialog`, never
  `window.open_dialog`. A mouse-opened dialog relies on the
  `prevent_default` inside that door. Openers check `dialog::can_open` before
  installing state: a duplicate kind would overwrite the covered dialog's draft.
  Object dialogs check `dialog::can_open_object` instead: they stack per domain,
  and `objectdialog::render::open` parks the covered state in its stack entry
  (`ShellModal::parked_object`) before installing its own. Code that must reach
  a covered object dialog (deliveries, reload refreshes, write reverts) iterates
  `object_dialog` plus `dialog::parked_objects_mut`. Unclaimed dialog-opening chords and the palette can open above a dialog;
  tile command lines, find prompts, and stack lists are refused while it is open.
- The pure state of a dialog is the truth; `dialog::sync_dialog_text`
  reconciles the shared input's text and focus after state transitions.
  Expression completion replaces the selected range directly to preserve input
  undo, then updates the object draft before synchronization.
- A multi-screen dialog registers its back step with `dialog::set_back`.
  The title row paints the Back button only while the step is available, and
  the step uses Escape's parent-stage transition. One Back click also discards
  any open field and filter query that earlier Escape presses would dismiss.
  Object-dialog confirmations block this transition until answered.
- Object edit stages select rows with row commands or nested editors through
  `Draft::is_cursor_stop`. Motion uses `move_selection`; resets use
  `settle_selection`. Lists with no eligible row retain the keyboard selection,
  while clicks on ineligible rows are ignored. Query and selection accessors
  must agree on which stages use the draft. See [cursor rules](../../docs/current/configuration-dialogs.md#stages-and-ownership).
- Every tile mouse-down path and every keyboard verb that moves tile focus
  re-arms `pending_focus_restore`. A module that drops a focused
  `InputState` must `window.blur(cx)` first, or every chord dies for the
  rest of the session.
- A page hides the tiles beneath it: `fill_active_tiles` yields nothing and
  `visible_tile_keys` is empty while one is open, so every occupant hears
  `set_visible(false)` and no flip barrier waits on a tile nobody can see.
  The context stack is `page` then the page's own context, never
  `workspace` or `tile`; `page::toggle_*` and `page::close` are refused
  under a modal; focus returns to the open page's handle (`focus_home`),
  not the shell root, when an overlay closes and after Escape or Enter in
  the scope field.
- Colors go through the doors: `chip_paint` for semantic chips on the main
  background, `chip_paint_on` on chrome and popovers, `row_paint` for list
  rows, and `control::paint` for hover and pressed states. Chip and control
  text clears 4.5:1 against its actual fill; active chip fills retain their
  separate 3:1 floor against the title bar. Each has a sweep over every
  bundled theme with no exception list.
- Chrome geometry is authored at the `Medium` rem through `scale::design`;
  radii come from the theme.
- Every gpui-kit and gpui-pre crate is `=`-pinned in the root `Cargo.toml`.
  "The pinned rev" in a comment means those versions, read from the
  registry source.
- Diagnostics request methods cannot notify observers themselves. Visibility
  and catalog-demand changes require a caller notification even though they
  leave diagnostic data versions unchanged. Explicit catalog demand survives
  the diagnostics page hiding.
- A stopped data thread is recorded once and never cleared: nothing restarts
  it, so its status segment stays until Geode restarts. The segment's text and
  tooltip are built in `note_thread_stopped`, not at paint. See
  [stopped threads and refusals](../../docs/current/shell.md#stopped-threads-and-refusals).
- Config-load diagnostics replace a batch; data conditions append separately.
  Keep their lifetimes distinct so a reload cannot hide a data-layer error.
