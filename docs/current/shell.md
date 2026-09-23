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
must be reconciled deliberately. A tile mouse-down and every keyboard command
that moves structural focus arms `pending_focus_restore`. The shell then
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

The session file records workspace trees, docks, stacks, focused regions,
module kinds with opaque module state, frame state, and palette usage. Parsing
heals local layout damage where possible: invalid dock state should not cost a
valid main workspace. A structurally invalid main tree causes a fresh session
with diagnostics rather than a partially trusted layout.

Runtime configuration writes target only the user layer. `config_write` is
the common door for ordered, atomic edits. Accepted writes to one directory
run in submission order on the background executor. The target file is parsed
before editing, temporary files live beside it and do not end in `.toml`, and
rename exposes either the old or new complete file to the reload poll.

Hot reload keeps the last valid configuration when a changed document is
rejected. Module-visible globals are updated only when their value changes so
observers do not repaint on every poll.

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
