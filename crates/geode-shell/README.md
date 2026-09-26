# geode-shell

The Geode shell: the tiling window manager, workspaces and docks, the
keymap engine, the command palette, the shared frame (scope, grouping,
as-of), theming, the modal dialogs, and the contract a module implements
to live in a tile. Built on gpui and gpui-component.

This crate never depends on `geode-data` or on any module crate. A module
gets shell-side handles only (a `TileId`, the frame entity, the action
registry); anything it needs from data it asks `geode-data` for itself,
and the two meet only in `geode-app`.

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
| `keymap` | Keystrokes, predicates, layered binding resolution, sequences, counts, and module fragments. See [keymaps and actions](../../docs/current/keymaps.md) for matching, filtering, and editor limitations. |
| `actions` | The shared action registry; the keymap maps keys to action ids and the palette lists them. |
| `frame` | The shared frame: scope with undo/redo, the active grouping slot, as-of, recent publishes, saved scopes, and the data and config generations, as one value every tile observes. |
| `scopebar`, `commandline`, `palette_usage`, `listfilter`, `choice`, `vimnav`, `vimfind`, `footer` | The models behind the scope bar, the per-tile `:` line, palette ranking (frecency), filtered lists, choice-with-typeahead, vim-style list motion and `/` find, and dialog footer hints. |
| `exprcomplete` | Pure suggestion state for a scope expression field: re-reads the caret position through `geode_core::scope::complete`, ranks rows with the shared fuzzy matcher, caps them at 50, and tracks a per-column categorical-values cache (`Loading`/`Ready`/`Failed`) keyed by request tag so a stale reply is dropped. Holds no gpui. |
| `dialogmode` | Shared Normal/Filter transitions. Keyboard and frozen-row clicks snapshot the query; Escape restores it and bare Enter keeps it, with neither exit acting on a row. See [dialog filtering](../../docs/current/shell.md#dialog-filtering) for focus and stage-specific exceptions. |
| `theme`, `fonts`, `fontsize`, `linenumbers`, `tileadd`, `tips` | Settings and their pure resolution rules: the bundled gpui-component themes, the bundled Inter and JetBrains Mono faces, the rem scale knob, `[ui] line_numbers`, the add-tile direction, tooltip text from the live keymap. |
| `config_write`, `keymap_edit`, `log_persist`, `reload` | Ordered user-layer writes, comment-preserving keyed edits, log-level persistence, and reload detection/rejection. See the [configuration contract](../../docs/current/configuration.md#runtime-edits) for write guarantees and the limits of keep-last-good. |
| `session` | Session encoding, local recovery, and atomic file replacement. Shell integration owns periodic snapshots and shutdown saves. See [session persistence](../../docs/current/shell.md#session-format) for recovery boundaries, unavailable modules, and write-failure behavior. |
| `diagnostics` | Source health, generations, independent config/data diagnostics, section versions, cached status summary, and watched/explicit catalog demand. See the [diagnostics contract](../../docs/current/shell.md#diagnostics-state-and-demand). |
| `perf` | The always-compiled frame-time histogram. |
| `defaults` | The builtin action set and keymap, the Builtin config layer. |

**Window integration**

| Module | Holds |
|---|---|
| `shell` | `ShellView`, the one view that owns the window: key dispatch (`input.rs`), tile occupants and focus restore (`occupants.rs`), rendering, drag and drop, the toolbar, sidebar and status bar, the palette, the settings, keybindings and object dialogs (`objectdialog/`), the dimension picker, the as-of dialog, the scope expression dialog and its whole/term/add modes (`scope_expr_view`), the toolbar's add-a-filter menu (`addfilter`), the choice dialog, the stack member list, which-key, hot reload, session I/O, a grid tile's selection footer strip (`aggregates`, prepared label/text pairs laid out, never formatted, at paint time), the color doors (`chip`, `listrow`, `control`, `colours`, `scale`), and `kbd`, the one door every on-screen key paints through (gpui-component's `Kbd`). Its tests live in `shell/tests/`. |
| `shell/expr_suggest` | The gpui controller and renderer for `exprcomplete::ExprCompletion`, shared by the frame's expression dialogs and the Scopes object dialog's open `expression` field: re-reads the shared `dialog_input` on any notify (a caret move refreshes it, not only a `Change`), requests categorical values under `shell::EXPR_KEY`, writes an accepted row as a range replace so cmd+z undoes it, and claims tab/shift-tab/up/down/ctrl-p/ctrl-n ahead of the field. `EXPR_KEY = QueryKey(u64::MAX - 4)` is reserved next to `PICKER_KEY`/`DIAGNOSTICS_KEY`/`SCOPES_KEY`; `ShellView::deliver_distinct` routes a reply there before any tile ever sees it. |
| `module` | The module-hosting contract: `TileContent`, `ModuleFactory`, `ModuleRoster`, `Delivery`, `StackHandle`. `module::recording` is the test double a downstream crate hosts a neighbour with. |
| `shell/objectdialog` | Domain drafts, staged editing, validation, overrides, and debounced application/persistence. See [configuration dialogs](../../docs/current/configuration-dialogs.md) for ownership and failure boundaries. |

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
  `prevent_default` inside that door.
- The pure state of a dialog is the truth; `dialog::sync_dialog_text` is
  the only thing that moves focus or writes the shared `Input`.
- A multi-screen dialog registers its back step with `dialog::set_back`.
  The title row paints the Back button only while the step is available, and
  the step must be the transition Escape's back step runs, so pointer and key
  cannot leave different screens.
- Object edit stages select rows with row commands or nested editors through
  `Draft::is_cursor_stop`. Motion uses `move_selection`; resets use
  `settle_selection`. Lists with no eligible row retain the keyboard selection,
  while clicks on ineligible rows are ignored. Query and selection accessors
  must agree on which stages use the draft. See [cursor rules](../../docs/current/configuration-dialogs.md#stages-and-ownership).
- Every tile mouse-down path and every keyboard verb that moves tile focus
  re-arms `pending_focus_restore`. A module that drops a focused
  `InputState` must `window.blur(cx)` first, or every chord dies for the
  rest of the session.
- Colors go through the doors: `chip_paint` for semantic chips,
  `row_paint` for list rows, `control::paint` for hover and pressed
  states. Each has a sweep over every bundled theme with no exception list.
- Chrome geometry is authored at the `Medium` rem through `scale::design`;
  radii come from the theme.
- Every gpui-kit and gpui-pre crate is `=`-pinned in the root `Cargo.toml`.
  "The pinned rev" in a comment means those versions, read from the
  registry source.
- Diagnostics request methods cannot notify observers themselves. Visibility
  and catalog-demand changes require a caller notification even though they
  leave diagnostic data versions unchanged. Explicit catalog demand survives
  the last diagnostics tile hiding.
- Config-load diagnostics replace a batch; data conditions append separately.
  Keep their lifetimes distinct so a reload cannot hide a data-layer error.
