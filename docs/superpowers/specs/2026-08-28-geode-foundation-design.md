# Geode Foundation Design

**Date:** 2026-08-28
**Status:** Approved pending review
**Governs:** platform architecture for all Geode development. Individual
modules (blotter, pricer, execution, …) get their own specs that must conform
to this document and to [the philosophy charter](../../PHILOSOPHY.md).

## 1. What Geode is

Geode is a desktop application for index exotic equity derivatives traders: a
single permanent tool that will grow to cover risk viewing, pricing display,
execution (routing orders to external algos), and data visualization. It is
built in Rust on gpui (UI framework) with gpui-component (component library).

Although a GUI application, it behaves like a TUI: fully keyboard-driven,
vim-style modal navigation, and i3/xmonad-style tiling window management.

**Users:** one trader today, the desk later. Multi-user means shared
binary + layered shared config (§8); there is no server tier and no
entitlement system of our own — filesystem permissions on the shared config
location are the access control.

**Platforms:** Windows is the primary deployment target; macOS is the
development environment. Both must work at all times. (gpui on Windows is
validated — a prior prototype confirmed it.)

**Data reality:** batch-refreshed, not streaming. Today: CSV files on a
network drive, refreshed on the order of minutes. Tomorrow: Parquet on a
share or an Iceberg data lake. Someday: streaming updates over Solace, and
federation of one dataset across multiple providers. Day one implements one
concrete source; the architecture makes the rest configuration-plus-adapter
work, not redesign (§5).

**Scale target:** hundreds of thousands of positions, headroom to ~1M rows,
across wide (100+ column) risk tables.

**Latency meaning:** because data is batchy, "ultra low latency" means
*interaction* latency. Regrouping, filtering, scoping, and navigating must be
perceived-instant regardless of data size (§7 budgets). Ingest cadence is not
the hot path; the render thread is.

## 2. Architecture overview

A single binary organized as a Cargo workspace with strict layering:

```
geode-app          the binary: wires shell + modules + services together
  ├─ geode-shell   tiling WM, workspaces, palette, keymap engine, scope/as-of
  │                state, theming
  ├─ geode-modules blotter, config editor, diagnostics (later: pricer,
  │                execution, charts…) — each its own crate
  ├─ geode-data    DataService: sources, ingestion, DuckDB, archive, query API
  └─ geode-core    shared vocabulary: types, config model, ids, errors,
                   perf utilities
```

**Dependency rules.** Modules depend on `shell` (to be hosted), `data` (to
query), and `core`. `shell` and `data` never depend on modules and never on
each other; `geode-app` is the only crate where everything meets. No module
may open a file or socket itself — it can only ask `DataService`. These rules
are enforced by crate visibility, not convention.

The `data` crate boundary is drawn so that a future split into a sidecar
data-daemon process (shared across windows/tools, surviving UI restarts) is
an evolution — swapping in-process calls for IPC — not a rewrite. That split
is explicitly **not** built now.

**Threading model.** Three tiers:

- **UI thread** — gpui's thread. Renders, handles input, owns entity state.
  Never touches a file, socket, or DuckDB connection; only ever reads
  *prepared* immutable snapshots.
- **Query pool** — background threads owning DuckDB read connections. Run
  view queries (regroup/filter/aggregate) and deliver results as immutable
  columnar snapshots via channels into gpui's async runtime.
- **Ingest** — background tasks that watch/poll sources, load into DuckDB
  staging tables via a dedicated writer connection, atomically publish new
  generations, and broadcast availability.

**Data flow, end to end:** source file → ingest adapter → DuckDB table
(generation-stamped) → view query (SQL compiled from a declarative view
definition + effective scope) → Arrow snapshot → virtualized table render.
The UI holds only the visible slice plus scroll headroom; a 1M-row table
costs the render thread nothing until scrolled to.

## 3. Interaction model

### 3.1 Tiling layer (i3 semantics)

A gpui window hosts numbered, nameable **workspaces** (`mod+1..9`). Each
workspace is a tree of splits whose leaves are **tiles**; each tile hosts one
module instance. Core verbs, chord-driven, i3 as the default map:

- split vertical/horizontal (`mod+v` / `mod+s`)
- focus movement (`mod+h/j/k/l`)
- move tile (`mod+shift+h/j/k/l`)
- resize mode (`mod+r`, then hjkl, `Esc` to exit)
- fullscreen tile (`mod+f`), close tile (`mod+shift+q`)
- tabbed/stacked container modes for dense workspaces

`mod` defaults to Alt on Windows; fully remappable. **Layouts** — the split
tree plus which module/view occupies each leaf, per workspace — are saveable
as named layouts (personal or desk-shared) and restored on startup.

### 3.2 Vim layer (inside a tile)

Focus is modal: the shell owns `mod+…` chords; the focused module interprets
everything else vim-style. For the blotter: `j/k` row motion, `h/l` column
motion, `gg/G`, `ctrl-d/u`, `/` incremental search with `n/N`, `zc/zo/za`
group collapse/expand, `v` visual row selection, `y` yank (rows as TSV to
clipboard), `:` opens a command line scoped to the tile (e.g.
`:group underlier,expiry`, `:filter delta > 1000`).

Modules declare motions and commands through a shared action registry — a
shell rule, not a convention — so idioms stay uniform across modules.

### 3.3 Command palette

`mod+p` (also `ctrl+shift+p`) opens a fuzzy palette over *everything
registered*: shell actions, module commands, saved views, layouts, scopes,
data sources, config files. Every entry shows its current binding, so the
palette doubles as the keymap's discoverability layer. A which-key-style hint
overlay after a held prefix is a nice-to-have, not v1-blocking.

### 3.4 Keymap engine

One declarative keymap file format, layered (built-in → desk → user), with
contexts (`workspace`, `tile`, `blotter && mode == normal`), chords, and
multi-key sequences. Modules never bind keys; they expose actions, and the
keymap maps keys to actions per context. This engine is pure logic, built
early, and exhaustively unit-tested (§10).

### 3.5 Mouse

Fully functional — click focus, drag splitters, header clicks to sort — but
never required, and never the only path to anything.

## 4. Global scope, filtering, and time travel

### 4.1 The scope model

A **scope** frames what every tile is looking at. Three predicate kinds,
composed with AND:

1. **Dimension selections** — structured multi-selects over configured
   fields: books, desks, underlyings, model codes, …. Each configured
   dimension gets a palette-style picker and appears as a chip in the scope
   bar.
2. **Text filter** — one string matched case-insensitively against all
   columns declared textual in the dataset schema; compiled to an OR of
   `ILIKE` predicates.
3. **Expression filter** — a typed predicate such as
   `model_code = 'EURP' and underlying = 'SPX'`, parsed against a restricted
   WHERE-clause grammar and validated against the schema, with inline errors
   at the caret. Not raw SQL.

### 4.2 Layering

Scopes stack: **global scope** (whole app) → **workspace scope** (optional,
per workspace) → **tile filters** (local to one view). The effective
predicate is the AND of all layers. A tile may be **unscoped** — pinned to
ignore upper layers (e.g. a whole-desk summary that must not shrink when the
trader zooms into one book) — and carries a clear visual marker so an
unscoped number is never misread.

### 4.3 Mechanics

Scope is observable shell state. `DataService` composes the effective
predicate into every view query, so a scope change is "requery all visible
views" — milliseconds at target scale. Scope changes are atomic per
generation of interaction: all tiles flip together, never a half-updated
screen. Scope clears are explicit and undoable.

Interaction surface: a persistent **scope bar** at the top of the window
(chips + text field), dimension pickers (e.g. `mod+b` for books), `mod+/`
for the global text filter, `:scope …` commands from any tile, and saved
scopes recallable from the palette and shareable as config.

### 4.4 Time travel (as-of)

An **as-of** selector joins scope in the shell's shared frame state, with the
same global/workspace layering. Setting as-of to 14:05 makes `DataService`
resolve, per dataset, the newest generation at or before 14:05 and route
queries to the archived table for that generation (§6). Same query path,
different table name — everything downstream is identical.

Datasets refresh on independent cadences, so the resolved state is "each
dataset as it stood at 14:05" — the question a trader is actually asking.
While as-of is active, a prominent, unmissable indicator marks the app as
historical; nothing on screen may look live when it is not. Returning to
live is one action, and undoable.

Diffing two as-of times ("what moved since the morning batch?") falls out of
this design cheaply and is noted as a future feature, not built now.

## 5. Data layer

`DataService` (in `geode-data`) is the only door to data.

### 5.1 Vocabulary

- **Source** — a configured origin: adapter + location + refresh schedule.
  Day one ships one adapter: *directory of CSVs on a network drive*, polled/
  watched. Parquet is the same adapter with a different reader; Iceberg
  arrives via DuckDB's native support; Solace later as a streaming adapter
  feeding the same tables. Sources are declared in config, not code.
- **Dataset** — a named logical table (`positions`, `risk`, …) with a
  declared schema. A dataset is *fed by* one or more sources — the
  federation seam. Multiple providers feeding one dataset is a config
  change: each source maps its columns to the dataset schema, rows carry a
  `source` column, conflicts resolve by declared precedence. Day one is
  1:1, but the indirection exists from the start.
- **Generation** — every refresh loads into a staging table, validates,
  then atomically swaps live and bumps a monotonic generation number with a
  timestamp. Open views are notified and requery. The UI can always show
  "as of 14:32:05, gen 47". A failed load never clobbers the last good
  generation; it surfaces as source-health degradation.
- **View definition** — declarative config: dataset + columns + derived
  columns (SQL expressions) + default grouping/aggregations + sort +
  formats. The blotter compiles one to SQL
  (`select … where <effective scope> group by <grouping> …`). Users create
  views via the UI or by writing config; both produce the same file.
- **Snapshot** — an immutable Arrow query result handed to the UI.
  Columnar end-to-end; the renderer reads column slices for visible rows.
  No row-object materialization anywhere.

### 5.2 Schema policy

Schemas are declared in config: names, types, which columns are textual (for
the global text filter), formatting hints. On load, mismatches degrade
rather than fail: extra source columns pass through; missing declared
columns become NULL with a specific health warning ("column `vega_1d`
missing in source X"). Tolerant of upstream churn, honest about it.

### 5.3 Storage engine

DuckDB, embedded, in-memory. A small pool of read connections serves view
queries; one dedicated writer connection serves ingest (DuckDB is
single-writer/multi-reader). Query cancellation is wired from day one: a
superseded query (user regrouped again before the last regroup finished) is
interrupted, not awaited.

## 6. Retention and the archive

**Two-tier storage, so live never pays for history.**

- The **live table** per dataset contains exactly the latest generation: no
  generation column, no history predicate, size independent of retention.
- On refresh, the previous live table is *renamed* into the archive
  (`positions@gen47` — metadata-only, effectively free) and the staging
  table takes its place.

**Retention** is per-dataset config: keep by count (`last 50 generations`)
and/or age (`72h`). An optional **spill tier** writes archived generations
older than a threshold to local-disk Parquet and drops them from memory —
still queryable through DuckDB, just colder. Spill ships configured-off
until memory measurement (diagnostics tracks memory by dataset) says it is
needed.

A background sweeper enforces retention; evictions are logged, and the
oldest available time per dataset is visible in the time-travel UI so users
can see how far back they can go.

## 7. Performance architecture

### 7.1 Budgets (contracts, not aspirations)

- **Input → pixel:** pure-UI actions (focus move, row navigation, mode
  change) render on the next frame: **< 8ms** (one frame at 120Hz).
- **Requery interactions** (regroup, refilter, scope change, as-of change):
  **< 50ms** end-to-end at 1M rows — query + snapshot handoff + first
  painted frame. 50–200ms gets a subtle progress affordance; slower is
  either a defect or a cold load with an honest progress UI.
- **Ingest invisibility:** a background refresh of any size may never drop
  a foreground frame.
- **Startup:** **< 1s** to interactive shell (layout restored, tiles
  showing loading state); data warms in behind. Never block interactivity
  on data.

### 7.2 Render discipline

Tables virtualize: only visible rows plus small overscan exist as UI
elements; scroll position indexes directly into columnar snapshots —
O(viewport), never O(rows). Snapshots are immutable and `Arc`-shared, so
handoff is a pointer swap. Cell formatting (number → styled string) is
computed once per snapshot per visible window and cached — never per frame.
High-repetition strings (book, underlier, model code) are interned at
ingest.

### 7.3 Concurrency discipline

Every data operation is cancellable and generation-tagged; a stale result
arriving after a newer request is dropped, never rendered. One in-flight
query per view with latest-wins coalescing: leaning on a regroup key five
times yields one query. Channels between tiers are bounded; backpressure
surfaces as source health, never as UI stall.

### 7.4 Enforcement

- Criterion benchmarks over the query/snapshot pipeline using a checked-in
  **synthetic data generator** (1M-row capable; no fixture files).
- Frame-time instrumentation always compiled in (cheap histograms), visible
  via a debug overlay toggle.
- CI fails on benchmark regression beyond threshold.
- Profiler support (Tracy or equivalent) from day one.

## 8. Configuration model

**What is config:** sources and datasets (with schemas), view definitions,
layouts, keymaps, saved scopes, themes, retention policies, dimension-picker
definitions. If a trader or desk lead could plausibly want it different, it
is config.

**Layers, deep-merged in order:**

1. **Built-in defaults** — compiled in; the app runs with zero files.
2. **Desk layer** — a directory on a shared network path, discovered via
   env var or a bootstrap file next to the exe. Desk-standard sources,
   schemas, views, layouts.
3. **User layer** — `%APPDATA%\geode\` on Windows (platform-appropriate
   elsewhere); personal overrides and personal artifacts.

Merge semantics are explicit and boring: maps merge by key, user wins;
named objects (a view, a layout) override whole-object by name — no partial
merging of a view. A palette command shows *effective* config with the layer
each value came from; layered config without provenance is a support
nightmare.

**Format: TOML everywhere** — hand-maintained and UI-written files alike.
One format, diffable, comments allowed. Every file carries
`config_version = 1` with explicit migration on load.

**Editing:** all config is editable both by hand and in the app. The
built-in config editor module (§9) opens any config file from the palette,
provides vim-modal editing with schema-aware inline validation, and saves
through the hot-reload path.

**Hot reload:** config files are watched. Safe changes (views, keymaps,
themes) apply live; unsafe ones (sources, schemas) prompt ("reload data
now?"). Invalid config never takes down a running app: errors go to the
diagnostics panel with file/line; last-good config stays active.

**Sharing flow:** "save view/layout/scope" asks *personal or desk*; desk
writes are gated by filesystem permissions — the network drive is the ACL.
The desk layer living in a git repo is recommended, not required.

## 9. Module system

### 9.1 The contract

A module is a crate exposing a `Module` implementation:

- **Identity & factory** — name, icon, constructor taking its config (e.g.
  which view definition to render) and handles to shell services:
  `DataService`, scope/as-of state, action registry, notifications.
  Everything a module touches arrives through this doorway.
- **Actions & commands** — declared into the shared registry at
  registration; the shell owns dispatch and keymap binding.
- **Tile lifecycle** — created into a tile; serializes state (view name,
  grouping, cursor…) into layout saves and restores from them; notified on
  hide/show so background tiles can drop subscriptions.
- **Frame subscription** — scope/as-of/generation changes arrive through
  one uniform mechanism, so cross-cutting behavior is identical in every
  module by construction.

Modules are compiled in and registered in a static list in `geode-app` —
the only place the app knows the roster. No dynamic plugins, no scripting
(the config-as-data layer is designed so an embedded scripting layer could
bolt on later if ever justified).

### 9.2 Day-one modules (v1)

1. **Blotter** — the flagship. Renders any view definition: grouping with
   collapsible group rows and aggregations, sorting, filtering, vim
   navigation, column operations (resize/reorder/hide, persisted to the
   view), visual selection and yank, cell formatting including red/green
   numerics and staleness styling.
2. **Config editor** — as in §8.
3. **Diagnostics** — source health, generations and timestamps, ingest
   errors, memory by dataset, frame-time overlay toggle, effective-config
   explainer. The honesty principle's home.

**Explicitly not in v1** (future modules on existing rails): charts/
data-viz, pricer panels, execution/order entry. Execution additionally
requires its own safety-focused spec (confirmation semantics, kill switch,
audit log) before any code is written.

## 10. Error handling, observability, testing

### 10.1 Error taxonomy

- **Data problems** (source unreachable, schema drift, stale, failed load):
  never modal, never fatal. Last-good generation stays live; affected tiles
  show staleness/health markers; details in diagnostics.
- **User errors** (bad expression, invalid config, unknown command):
  immediate, inline, local — error at the point of entry with file/line or
  caret. Never a dialog for a typo.
- **Bugs:** background threads have per-subsystem panic boundaries — a
  crashed ingest or query worker restarts, logs loudly, and marks its
  source/view degraded. Only a render-thread panic takes the app down, and
  that writes a crash report (recent actions + log tail).

### 10.2 Observability

Structured `tracing` logging with per-subsystem levels, ring-buffered in
memory (viewable in diagnostics) and written to local rotating files. Perf
metrics (frame times, query latencies, ingest durations, memory by dataset)
feed the same diagnostics module. No telemetry leaves the machine.

### 10.3 Testing strategy

Test weight goes: data layer ≫ shell logic ≫ module interaction; visual
polish is verified by humans.

- **Data layer:** unit/integration tests over adapter → DuckDB → query →
  snapshot round-trips with synthetic fixtures; generation/retention/
  time-travel semantics; scope predicate compilation under property tests
  (generated scope stacks always compose to valid, correct SQL); schema
  drift cases.
- **Shell logic:** pure-logic unit tests — tiling tree operations, keymap
  resolution (contexts, layering, chords), config merging with provenance.
  All designed to be testable without a window.
- **Modules:** gpui `TestAppContext` — simulated keystrokes through real
  focus/dispatch ("j moves cursor", ":group book regroups", "scope change
  requeries visible tiles exactly once").
- **Performance:** the §7.4 benchmarks, CI-gated.
- **Eyeball layer:** a `--demo` flag boots the app on generated data with
  no real sources — for manual testing, screenshots, and perf work
  anywhere.

TDD applies during implementation per house rules.

## 11. Risks

- **gpui API churn** — unreleased, moving framework. Pin a known-good
  revision; upgrade deliberately on a branch; keep gpui-touching code in
  `shell` and module render layers so churn never reaches `data`.
- **Table widget adequacy** — gpui-component's `DataTable` (its most
  capable virtualized table) is the starting point, but our blotter's
  needs (group rows, collapse, vim cursor, snapshot formatting cache) may
  outgrow it. Build the blotter behind our own table abstraction; start on
  `DataTable`; budget for a custom low-level table Element if measurement
  demands it. This is the most likely custom-Element work.
- **Archive memory growth** — wide tables surprise. Memory-by-dataset
  metric from day one; spill tier designed and ready, enabled by config.
- **Network drive pathology** — slow/hung reads on Windows shares. Ingest
  reads carry timeouts and run on threads nothing waits on; slow sources
  degrade health visibly.
- **Keymap engine complexity** — the subtlest pure-logic component (chords
  + modes + contexts + layering). Built early, exhaustively tested, and no
  module ever binds keys outside it.

## 12. Build phases

Each phase gets its own implementation plan; later phases get their own
specs first.

- **Phase 0 — Skeleton:** Cargo workspace, crate boundaries, CI, benchmark
  harness, demo-data generator, empty shell window running on Windows and
  macOS.
- **Phase 1 — Shell:** tiling WM, keymap engine, palette, config layering
  and hot reload, theming.
- **Phase 2 — Data:** DataService, CSV adapter, generations, retention and
  archive, scope compilation, snapshots, benchmarks.
- **Phase 3 — Blotter:** the flagship module — view definitions, grouping,
  vim navigation.
- **Phase 4 — Frame features:** scope bar UI, time travel, diagnostics
  module, config editor.

Desk-shareability (config layering) exists from phase 1; nothing about
multi-user blocks v1.
