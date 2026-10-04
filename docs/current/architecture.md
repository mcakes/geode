# Architecture

Geode is a desktop shell for independently owned desk capabilities. The code
is arranged so the UI, data service, and domain modules communicate through
small contracts rather than reaching into one another's state.

## Dependency direction

Dependencies point toward smaller and more stable crates:

```text
                         geode-app
                 composition and process setup
                  /           |        \          \
          feature modules     |     geode-compose  |
            |       \         |   (gpui-free half) |
            |    geode-tile   |         |          |
            |       /         |         ▼          ▼
            └──► geode-shell ◄┘        geode-data ◄┘
                     |                       |
               geode-widgets                 |
                     |                       |
                     └─────► geode-core ◄────┘

calculation leaf: geode-pricing ─────────────────► geode-core
pure presentation: geode-chart, geode-widgets ───► geode-core
wire formats: geode-documents ───────────────────► geode-core
```

A feature module depends on `geode-shell` directly (the `TileContent`
contract, key chips, the rem scale) as well as through `geode-tile`, and may
also name `geode-widgets`, `geode-chart` and `geode-core` directly.
`geode-diagnostics` is a page rather than a tile. It uses `geode-tile::motion`
for row navigation and does not participate in tile flip barriers.
`geode-guide` is an offline tile over the bundled user guide. It depends on
the shell and shared tile mechanisms, owns no data handle, and acknowledges
frame flips immediately.

`geode-core` is shared vocabulary without window, database, or network
ownership. Typed interpretation and merging are I/O-free; its configuration
loader reads disk documents through `Config::read_docs` and `Config::load`.
Types shared across a forbidden dependency boundary live there: scopes,
link groups, schema, query outcomes, snapshots, health, document rows, series
requests, pricing requests, vol-slice requests, and text file requests and
outcomes (`textfile`). `geode_core::classification` is the pure editing model
for a classification (a derived dimension a person edits): grid rows, edits
that return the whole next object with an undo entry, CSV import and export
planning, and the rules for naming one and choosing its source. It reads and
writes nothing; the bytes come and go through the data service, and the
object is written through the shell's config door.

`geode-shell` owns the window and interaction model. It does not depend on the
data service or on feature modules. `geode-data` owns sources, DuckDB, and
background data work. It does not depend on the shell or feature modules.

`geode-tile` is the kit tiles are built from: the popover, the `.` action
menu, the in-tile y/n confirm and the notice line, as models with one
painter each, the `colour` cache (`ColourCache`/`Resolved`: one resolve per
named color per theme input, shared by the blotter and pricer cell
painters), and the `following` flip-barrier state machine every following
tile runs for its own query. It depends on `geode-shell` for its paint doors
and the live keymap, never on `geode-data` or a feature module, and the shell
never depends on it. A tile mechanism two modules would otherwise each write
lives there. The menu and popover implementations live in `geode-shell`,
which also uses them for its row menu; `geode-tile` re-exports them for tiles.

Feature crates such as `geode-blotter`, `geode-marketdata`,
`geode-timeseries`, `geode-volslice`, `geode-classifications`, and
`geode-pricer` implement the shell's module contract and may ask the data
service through `DataHandle`. They do not depend on sibling features.
`geode-compose` builds the engine configuration: the schema with the app's
datasets pinned, sources, dimensions, document kinds, the clock and the
adapters. `geode-app` adds the UI-facing services: it registers module
factories (one market-data factory per accepted `panels` entry), kind
actions, and pricers, and opens the window. Market-data panels are
configuration checked there against the registered document kinds and kind
actions; a kind action's behavior is code in the module that dispatches it.
Diagnostics implements `PageFactory` and `PageContent`; `geode-nemo` and
`geode-positions` implement row-menu `DimensionAction`s. The app registers
these alongside the tile factories.

`geode-compose` is the part of composition a headless process can use. It
holds the builtin documents that decide the store (`builtin_data_layer`:
the app's `pricer_sheets` and `pricer` declarations, which live in
`geode_core::builtin`, plus the `--demo` layer), `engine_setup`, the store
and config paths, and the demo transports. It never depends on gpui. The
app's builtin layer is its own documents plus the data layer, so a process
built from the data layer alone and the same desk and user directories has
the app's schema and sources; `geode-app` tests that contract.

Reusable presentation is kept below features. `geode-widgets` holds shared
stateful controls; `geode-chart` holds chart preparation and painting. It is
a kit and one element per chart type, never one per module: an element
borrows the kit and owns its model and paint, and a module prepares that
element's model. Wire formats live in `geode-documents`. Calculation crates
are leaves reached through request and outcome values, so linking a
calculator into the process does not let a module call it directly.

## Runtime ownership

The UI thread owns GPUI entities, rendering, input dispatch, and prepared
presentation. It reads immutable or retained values and never performs data
I/O. The main window contains one `ShellView`, wrapped by gpui-component's
`Root` for window-level component facilities.

`DataService` runs on its own thread and accepts bounded, nonblocking requests
through `DataHandle`. One ingest runner owns the DuckDB writer. A read pool
owns independent read connections. Source adapters, discovery, subscription,
fetch, pricing, egress, and logging workers communicate through bounded
channels or explicit sinks. Egress hands each document's rows to a
per-target worker, which encodes and sends them off the service thread. Every
long-lived data-service thread is supervised: one that dies is declared once
to the UI and never restarted. The text file worker (`geode-files`) is the
one piece of file I/O `geode-data` performs that is not a source: modules
never touch the filesystem, so a tile's import or export is a request
answered by tile key and tag, and a slow path stalls neither the UI nor the
request loop (see [text files](data-path.md#text-files)). Transport threads
standing in for a vendor client (the channel adapter's dispatcher, the demo
bus) are not.

Submission reports admission or refusal without waiting for queue space; a
refusal says whether the queue was busy (a retry can succeed) or the service
has stopped (none can).
Admission does not guarantee completion: cancellation, supersession, startup
failure, and worker failure have request-specific effects. Callers handle
refusal explicitly and check outcome freshness. See
[requests and UI delivery](request-delivery.md) for these boundaries.

See [the shell](shell.md) and [the data path](data-path.md) for the detailed
lifecycle and routing contracts.

## Data and presentation

Columnar, immutable snapshots cross from the data layer to modules. A module
prepares the model its renderer needs when data or settings change; a frame
should mostly perform indexed reads and compose elements. Large collections
use struct-of-arrays storage and virtualized presentation.

The frame holds a shared scope, grouping, and as-of, plus a separate copy for
each pinned workspace. Tiles may override parts of their workspace's state
or follow a link group's scope. Version counters let a module decide
which changes require a query or rebuild without comparing whole documents.

A module receives the frame as a `FrameRef`, not the bare frame entity. A
tile's `FrameRef` is bound to the tile and its workspace for life; reads
resolve to that workspace's lane, with the scope of the link group the tile
follows in place of the lane's (see
[the shared frame](shell.md#the-shared-frame)).

`geode_core::link` is the link-group vocabulary the shell and the modules
share: the four groups, a tile's membership, the `Emission` (a scope and
board entries) a module answers, and the one column a group's scope is named
by. It is pure. The frame in `geode-shell` holds every group's scope and
board and every tile's membership; a module owns none of it, stores no
group, and has no door to a group: the frame's membership and emission
writes are private to the shell crate, and a module's only write for a group
is `set_scope` / `clear_scope` through its own handle while it follows one.
Emission is a pull: a module says its emission may have changed through a
callback that carries nothing, and the shell reads `TileContent::emission()`
and posts it (see [link groups](shell.md#link-groups)).

Financial calculation is outside the application layer. An in-process
calculation crate remains behind the same request/outcome seam that an external
service could implement. The app may shape results through grouping,
filtering, aggregation, and joins; domain algorithms remain upstream leaves.

## Configuration and persistence

Configuration is TOML layered in this order:

1. compiled builtin defaults;
2. a shared desk directory;
3. the user's configuration directory.

Documents merge recursively with whole-object exceptions and retain
provenance. Reload retains the active configuration for errors collected
before its acceptance decision; later typed-reader failures do not roll back
the whole candidate. Runtime edits write the user layer through an ordered,
atomic write path. A module never writes configuration itself: it queues
whole-object edits through its frame handle (`FrameRef::queue_config_edits`), and the
shell folds them into the same debounced, user-layer batch the configuration
dialogs use (see [the config door](shell.md#the-config-door)). The session
file holds layout, occupants, frame state, and palette usage, with separate
save ordering and recovery rules. See
[configuration](configuration.md) and [session persistence](shell.md#session-format)
for those boundaries.

Configuration is a public interface. File order can carry meaning, action IDs
and key bindings are user facing, and an existing database is not implicitly
migrated when a schema document changes.

## Failure boundaries

Expected failures become data: diagnostics, health, refused submissions, or
per-request errors. The request loop contains each request, so a panicking
request is answered once with an error through its own completion route and
the loop serves on. Read, pricing, ingest, and egress workers contain panics
in their operation paths; egress contains encoding and transport separately
and continues serving its queue. Work nobody is waiting on — identity
listings, the stale check, the local sweep, discovery, result delivery —
reports its panics as diagnostics or health. A thread that unwinds past every
boundary is declared as `DataEvent::ThreadStopped`, shown in the status bar
until restart, and never restarted. Containment does not interrupt blocked
calls. The application panic hook logs panics marked by those containment
boundaries without writing a report. Other panics, including a supervised
thread's death, trigger a best-effort report under the user config directory
before the previous hook runs. An absent marker does not establish whether
the process will exit; another caller may catch the unwind. See
[containment and liveness](data-path.md#containment-and-liveness).

Health and freshness describe what the system knows rather than concealing
degradation. A source can remain queryable while degraded; the UI must retain
the reason and timestamp needed to interpret its values.

## Performance contracts

The architecture keeps expensive work outside render by construction:

- pure UI actions target one frame, with an 8 ms budget;
- a view requery at one million rows targets 50 ms;
- ingest must not stall the UI or discard a foreground result;
- hot paths avoid per-frame allocation and repeated formatting;
- benchmarks keep debug symbols so measured regressions can be profiled.

Maintained budgets and known gaps belong in [performance.md](performance.md).
Raw results, hardware, and fixture conditions belong in the measurement log.
A budget claim without its measurement conditions is not evidence.

## Verification

Pure state and compiler behavior are tested below the window. GPUI context and
window tests cover retained state, focus, keyboard, pointer, and overlay
contracts. Benchmarks cover named latency paths. Targeted mutation checks are
used where a green outcome test can still miss silent wrong-data behavior.

CI formats, lints, tests, compiles benchmarks, and builds the shell's
`test-support` feature on macOS and Windows. Visual behavior that a headless
window cannot expose remains a separate display check and should be recorded
as a limitation in the relevant current guide.

## Where details live

- [The shell](shell.md): window state, tiles, input, focus, modules, dialogs,
  and session persistence.
- [Tiling](tiling.md) and [keymaps](keymaps.md): pure layout and input contracts.
- [Typed documents](typed-documents.md) and [configuration dialogs](configuration-dialogs.md):
  validation, draft application, and persistence.
- [The data path](data-path.md): ingestion, storage, querying, time travel,
  freshness, and health.
- Crate READMEs: local module maps, commands, and narrow invariants.
- [Philosophy](../PHILOSOPHY.md): product principles that constrain all of
  the above.

The dated implementation archive is useful when a current explanation omits a
tradeoff. It is not needed to establish how the system works today.
