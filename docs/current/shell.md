# The shell

The shell is Geode's persistent workspace. It owns the window, tile layout,
keyboard routing, shared frame state, dialogs, configuration surfaces, and the
contract used to host modules. Feature crates own their domain behavior; the
shell gives them a place to live and common interaction rules.

The shell never depends on `geode-data` or on a module crate. `geode-app`
composes those layers. This dependency boundary lets a module carry its own
data handle while receiving only shell concepts such as `TileId`, `Frame`, and
`Delivery` from the host.

## State ownership

`ShellView` is the retained GPUI entity for one window. It owns transient
window state: the active workspace, tile occupants, focus restoration,
palette, modal, command line, drag state, and session dirt. Its `render`
method describes the current frame; it must not become a second domain model
or perform blocking work.

State with a narrower owner stays outside `ShellView`:

- `Workspaces` and the tiling tree own layout and structural focus.
- `Frame` owns the scope, grouping, as-of value, recent publications, and
  version counters observed by tiles.
- Each module entity owns its cursor, subscriptions, draft, and prepared
  presentation.
- GPUI component state, such as `InputState` and `TableState`, owns reusable
  control behavior.
- Four GPUI globals carry settings that are genuinely app wide and visible to
  modules: `UiSettings`, `Chords`, `AppClock`, and `SeriesSettings`.

New state belongs in the narrowest owner that can keep it correct. A global is
appropriate only when independently hosted modules must observe the same
application setting.

## Tiles, workspaces, and stacks

The pure tiling model has three node kinds: split, leaf, and stack. Layout is
computed once by `Tree::layout`; painting, directional focus, divider
geometry, and drop targets use that answer rather than maintaining parallel
geometry. Each workspace also owns left, right, and bottom dock trees.

See [tiling and workspaces](tiling.md) for region crossing, empty-dock focus,
insertion/removal rules, drag geometry, and restoration limits.

A stack occupies one layout slot and keeps two or more leaf tiles alive. Only
its active member is visible. Focusing a member activates it, so structural
focus cannot point at a hidden member. The shell sends each occupant a
`StackHandle`; modules paint its marker through the shared builder and use it
to open the shell-owned member list.

Tile occupants are created through the app-supplied `ModuleRoster`. A new
occupant begins hidden and receives an explicit visibility value during the
next reconciliation. Hidden occupants may release live subscriptions and
must requery when shown if their followed versions changed.

## Actions and keyboard routing

The action registry is the vocabulary shared by the keymap and command
palette. The keymap resolves layered bindings against an ordered stack of
contexts and supports chords, sequences, and numeric counts. The focused
surface contributes its context; module fragments extend the vocabulary
without giving a feature access to `ShellView`.

See [input and dialogs](input-and-dialogs.md) for routing before the matcher,
focused text ownership, palette and completion behavior, and frame pickers.

The [keymap contract](keymaps.md) defines precedence, immediate exact matches,
sequence cancellation, count handling, and the limits of fragment filtering
and binding-editor resolution.

One logical command has one action or owner method. Keyboard bindings, palette
rows, toolbar controls, and pointer handlers route to that command instead of
copying its mutation. Every pointer command exposed to the user must have a
keyboard route.

The per-tile `:` command line is local to that tile. It may change the tile's
query, presentation, cursor, or draft. Frame-wide and application-wide
commands belong in the action registry and palette. `/` is likewise routed to
the focused occupant through `FindEvent`.

## Focus

GPUI window focus and the tiling model's focused tile are separate state and
must be reconciled deliberately. A tile mouse-down (a right press focuses
exactly as a left one does, so a module's context menu opens in the tile whose
keys it answers to, but arms no drag or double-click gesture) and every
keyboard command that moves structural focus arms `pending_focus_restore`. The shell then
returns window focus to the appropriate tile surface, except while that same
occupant intentionally holds an insert-mode input.

An occupant that closes a focused input must blur it before dropping its
handle. Switching workspaces also restores focus immediately when the old
occupant remains alive but is no longer mounted. Without these steps GPUI can
retain or fall back from a handle that no longer receives the shell's key
listeners, leaving subsequent chords ineffective.

Dialogs open through `open_shell_dialog`. That door closes competing
transient surfaces, establishes the modal state, and calls `prevent_default`
so a dialog opened from mouse-down keeps the focus it assigns. Dialog content
is built from a borrowed `&ShellView` during render; it must not re-enter the
entity with `Entity<ShellView>::read` while that entity is already rendering.

## Dialog filtering

Object dialogs (Browse, Edit, Column, and Values), Settings, and Keybindings
share `dialogmode`'s filter transitions. `/` and a click on the frozen filter
row both record the current query before focusing the input. Escape restores
that entry query; bare Enter keeps the edited query. Both leave Filter mode
without opening a row, committing an edit, starting key capture, or closing
the dialog. A second Enter in Normal mode performs the selected row's normal
action where supported: opening an object or nested editor, opening a Settings
choice, or starting a Keybindings capture.

`enter_filter`, `filter_exit`, and `exit_filter` own the snapshot and key rules;
`sync_dialog_text` reconciles the resulting query and focus. Escape accepts
modifiers, while the keep-query Enter must be unmodified. Restoring a different
query resets selection and scroll to the first match; leaving unchanged text
keeps selection. In Normal mode, subsequent Escape presses clear a query,
return from a nested stage, then close the dialog.

Naming, open object value fields, Settings typeahead, and keybinding capture
have their own commit/cancel handling before filter routing. They can focus
the same input or use the same mode value without adopting filter-exit
semantics. Palette, dimension picker, and as-of surfaces likewise keep their
own interaction rules. See [configuration dialogs](configuration-dialogs.md)
and [keybinding editing](keymaps.md#editing-unbinding-and-reset).

## The shared frame

`Frame` is a pure value held in a GPUI entity. It combines the global scope,
active grouping, as-of value, recent publications, saved scopes, and version
counters. Every mutation bumps only the counters affected by that change, so
a tile can cheaply ignore dimensions it does not follow.

Dataset and document watches narrow publication invalidation to consumers
that read the affected data. The frame keeps weak watches, allowing closed
tiles to disappear without explicit deregistration.

A scope, grouping, or as-of change opens a flip barrier. Following tiles stage
their results until all participants answer or the deadline passes, then
promote together. This prevents one frame from showing tiles evaluated under
different global states. A later frame change replaces the barrier; an old
result cannot satisfy the new version tuple.

Scope text editing is one undoable session. The first real change records the
base scope, subsequent keystrokes coalesce, and returning exactly to the base
removes the no-op undo entry. Changes made through another surface during the
session remain distinct.

## Module hosting and delivery

`TileContent` is the module boundary. An occupant supplies its key context,
handles actions and local commands, receives find events and deliveries,
reports focus ownership, and accepts visibility and stack state. Required
methods make lifecycle obligations explicit for every feature.

`Delivery` is an exhaustive enum. Adding a new outcome type forces every
occupant to decide how it handles that variant at compile time. Query, pricing,
and series outcomes route by tile key. A series fetch completion has no tile
key and is broadcast to visible occupants because several tiles may watch the
same `(identity, source)` pair.

The app bridge supplies these deliveries from a coalescing mailbox. See
[requests and UI delivery](request-delivery.md) for admission/refusal,
stale-result handling, publication routing, and window lifetime.

## Diagnostics state and demand

[`Diagnostics`](../../crates/geode-shell/src/diagnostics.rs) holds operational
state beside the frame. The app bridge supplies data events; the shell
supplies config-load diagnostics. The model does no I/O or clock reads and
has no GPUI context. Callers supply timestamps and notify after mutation.
The [diagnostics tile](features.md#diagnostics) prepares its own visible rows
from this state.

The combined `version` invalidates the cached status summary. `DiagVersions`
provides narrower counters so each tile can ignore unrelated updates:

| Counter | Changes |
|---|---|
| `sources` | Source description, health/detail, poll times, ingest activity |
| `data` | Publication events and catalog snapshots |
| `config` | Current config-load batch, batch history, retained data conditions |
| `log_levels` | Target-level settings |
| `perf` | Copied frame histogram and dropped-event count |

Equal snapshots leave their counters unchanged; loading and publication events
always advance theirs. Frame as-of/config versions and log ring sequences are
separate inputs observed by the tile. Catalog resource metrics and frame
requery statistics have no dedicated perf invalidation, so they appear on the
next perf-section rebuild.

Source health remains absent until the first report, even if description or
poll events created the source entry. Unreported sources do not contribute to
the status summary. A changed health or detail records a transition, including
recovery, with the most recent 16 retained per source.

Config loads replace the current diagnostic batch; a clean load clears it and
an identical load adds no history. The model retains the latest 16 changed
batches, including the current one. Data-layer conditions append separately,
deduplicate against retained entries, and keep at most 256. Config reloads do
not clear them, and they have no per-condition resolution operation. The
summary counts current config errors and retained data errors independently;
history is excluded. Cache hits share an `Rc<str>` without copying text.

Catalog demand has two lifetimes. `watch` registers a visible tile and queues
its initial refresh; publications and visible as-of changes queue watched
refreshes. The last `unwatch` clears that pending demand. `request_catalog`
records an explicit consumer's request, which survives diagnostics hiding.
When both are pending, `take_catalog_request` consumes them together as
explicit demand, preserving its retry policy.

Demand changes do not advance data versions. Callers must still notify in the
same entity update so the bridge observes them. The bridge allows one catalog
request in flight, coalesces follow-up demand, and retries refused or failed
requests with a delay. It reads the current frame as-of when submitting.

Histogram copying is limited to watched diagnostics and changes in sample
count or maximum. This keeps idle polling from causing needless repaints,
but misses changes solely to discarded-idle counts or a reset/refill that
reproduces the same count and maximum.

## Persistence and configuration

### Session format

`session.toml` in the user configuration directory stores layout and transient
state. It is loaded once at startup and excluded from configuration reload
change detection. Theme and other durable preferences live in layered config.
With no configured session path, the shell skips persistence.

The writer emits `config_version = 1` and these records:

| Record | Contents |
|---|---|
| `active` | Workspace index, 1–9 |
| `workspaces.N` | Main tree, focused tile, optional fullscreen tile, and focused region |
| `workspaces.N.docks.<side>` | Left, right, or bottom dock tree, focused tile, visibility, and size |
| `workspaces.N.tiles.<id>` | Module name and its opaque state table |
| `frame` | Dimension selections, text/expression scope, grouping slot, and as-of |
| `palette.usage` | Per-row usage count and last-used timestamp |

Trees use recursive `leaf`, `split`, and `stack` nodes. Splits store orientation,
children, and ratios; stacks store tile IDs and the active member index. All
materialized workspaces are saved, including empty ones. Default docks, empty
module state, and empty usage history are omitted. Only tile records whose IDs
belong to the workspace's main or dock trees are written.

The legacy dock `tile = N` encoding loads silently as a single-leaf tree. If
both `tile` and `node` appear, `node` wins with a warning. Unknown keys are
ignored during loading and lost on the next save; serialization reconstructs
the document and does not preserve comments. See
[`session.rs`](../../crates/geode-shell/src/session.rs) for the node encoding.

### Recovery and restoration

A missing file starts a fresh session without warnings. An unreadable file,
invalid TOML, or rejected session starts fresh with warnings logged by the app.
A missing version warns and assumes version 1; any other present version value
rejects the session. Loading and recovery do not rewrite or back up the file;
a later save replaces it with the current state.

| Problem | Recovery |
|---|---|
| Invalid workspace index, workspace shape, or main-tree structure | Reject the entire session, including tile records, frame state, and usage |
| Positive finite split ratios with valid arity | Normalize the ratios |
| Repeated/already-claimed stack members, invalid active index, or fewer than two surviving members | Prune members, reset the index, and collapse or remove the stack as needed |
| Dangling focus or fullscreen ID | Drop the reference; give a nonempty tree a valid focused tile |
| Invalid dock subtree | Drop that dock's tree with a warning; retain the workspace |
| Duplicate dock tile claims | Main trees take priority, then earlier docks; prune duplicate dock claims with warnings |
| Hidden focused dock | Choose a fallback region; visible empty docks remain valid focus targets |
| Main fullscreen combined with dock focus | Clear fullscreen and keep dock focus, with a warning |
| Malformed or locally dangling tile record | Warn and drop the record |
| Invalid optional frame or palette data | Retain usable fields/entries and warn for the errors their readers report |

Frame restoration parses scope expressions but does not validate columns
against the current schema. Invalid slots, expression syntax, date strings,
and non-array dimension entries warn; some wrong-type fields and non-string
dimension values are silently ignored. `Scope::impossible` is not persisted.
The shell applies scope, slot, and as-of, then clears scope history. Undo/redo
history and recent publishes start fresh; saved scopes come from config.

The shell creates occupants from restored records through the module roster.
Only the factory matching a record's module name receives its state. An
unavailable module displays a placeholder and retains the original record for
subsequent saves. Closing the tile discards it; filling the placeholder replaces
it with the new occupant's state. Restored IDs seed subsequent tile allocation
so ordinary additions do not reuse them.

### Saving and failure behavior

Workspace actions mark layout state dirty and return without session file I/O.
The shared reload watcher checks for a session snapshot on its 500 ms tick,
before any configuration-scan early return. It also compares serialized module
state, frame versions, and palette usage versions, so those changes can trigger
a save independently of layout dirt. This is periodic coalescing, not a timer
reset after each action; other work adds to the interval.

Snapshot collection and TOML serialization run on the UI thread. The watcher
awaits the file write on the background executor before continuing its loop.
The layout dirty flag clears before serialization, and comparison baselines
advance when serialization succeeds, before disk I/O. A failed write is logged
but does not retry unchanged state on the next tick. A serialization failure
leaves comparison baselines unchanged, but a layout-only change can lose its
dirty flag. These baselines track extracted snapshots, not confirmed saves.

The quit hook saves current state synchronously regardless of dirty flags.
Session writes use the shared atomic replacement primitive: a unique sibling
temporary file, file sync, then rename. Readers see a complete old or new file;
the directory is not synced, so durability across system failure is best-effort.
Errors can leave temporary files. Session writes bypass the configuration
submission queue and transaction lock. The quit hook does not join an in-flight
periodic save, so the last rename can leave an older snapshot on disk. Separate
processes writing the same session path likewise have no ordering guarantee.

### Configuration writes and reloads

The [configuration-dialog contract](configuration-dialogs.md) covers draft
ownership, stages, validation, inherited objects, and the boundary between
local application and successful persistence.

Runtime configuration writes target only the user layer. `config_write` is
the common door for ordered, atomic edits. Accepted writes to one directory
run in submission order on the background executor. The target file is parsed
before editing, temporary files live beside it and do not end in `.toml`, and
rename exposes either the old or new complete file to the reload poll.

Hot reload keeps the last valid configuration when a changed document is
rejected by the file, modifier, clock, or keymap checks. Later typed readers
do not roll back the whole reload. Every accepted reload advances the frame's
config revision and republishes `Chords`; the other module-visible globals
update only when their values change. See the
[reload contract](configuration.md#hot-reload) for detection, validation,
notification ordering, and restart requirements.

## Presentation rules

Shared builders own repeated shell presentation: semantic chips, list rows,
control interaction states, colours, and the rem-based geometry scale. Use
stable domain-derived element IDs. Stateful inputs, lists, and tables are
created once and retained rather than rebuilt during render.

Theme values provide colors and radii. Chrome geometry is authored against
the rem scale so application zoom changes the interface coherently. Prepared
models and caches must include every value that can change their output,
including theme or rem revisions where geometry is involved.

## Verification and limitations

Pure tests cover tiling, key resolution, frame transitions, parsing, and
ranking. GPUI context and window tests cover focus, keyboard and pointer
routes, dialogs, occupants, and persistence. Tests should exercise the public
interaction route for focus-sensitive behavior; calling an internal mutation
does not prove that keys reach it.

The `test-support` feature exposes the recording occupant and test accessors
used by feature crates. CI builds that feature explicitly. Visual facts still
require a real-window display check when headless tests cannot observe color,
exact geometry, or animation.

See the [geode-shell crate README](../../crates/geode-shell/README.md) for the
module map and commands.
