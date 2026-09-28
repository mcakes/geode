# Architecture

Geode is a desktop shell for independently owned desk capabilities. The code
is arranged so the UI, data service, and domain modules communicate through
small contracts rather than reaching into one another's state.

## Dependency direction

Dependencies point toward smaller and more stable crates:

```text
                         geode-app
                 composition and process setup
                  /           |             \
          feature modules     |          geode-data
            |       \         |              |
            |    geode-tile   |              |
            |       /         |              |
            └──► geode-shell ◄┘              |
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
`geode-diagnostics` has no popover, menu, confirm or notice line; it depends
on `geode-tile` only for the flip-barrier arrival.

`geode-core` is shared vocabulary without window, database, or network
ownership. Typed interpretation and merging are I/O-free; its configuration
loader reads disk documents through `Config::read_docs` and `Config::load`.
Types shared across a forbidden dependency boundary live there: scopes,
schema, query outcomes, snapshots, health, document rows, series requests,
and pricing requests.

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
lives there.

Feature crates such as `geode-blotter`, `geode-marketdata`,
`geode-timeseries`, `geode-diagnostics`, and `geode-pricer` implement the
shell's module contract and may ask the data service through `DataHandle`.
They do not depend on sibling features. `geode-app` constructs shared
services, registers module factories, adapters, document kinds, and pricers,
and opens the window.

Reusable presentation is kept below features. `geode-widgets` holds shared
stateful controls; `geode-chart` holds chart preparation and painting. Wire
formats live in `geode-documents`. Calculation crates are leaves reached
through request and outcome values, so linking a calculator into the process
does not let a module call it directly.

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
to the UI and never restarted. Transport threads standing in for a vendor
client (the channel adapter's dispatcher, the demo bus) are not.

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

Shared global frame state contains scope, grouping, and as-of. Each tile may
follow or override parts of that state. Version counters let a module decide
which changes require a query or rebuild without comparing whole documents.

A module receives the frame as a `FrameRef`, not the bare frame entity. A
tile's `FrameRef` is bound to its workspace for life; reads resolve to that
workspace's lane (see [the shared frame](shell.md#the-shared-frame)).

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
atomic write path. The session file holds layout, occupants, frame state,
and palette usage, with separate save ordering and recovery rules. See
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
