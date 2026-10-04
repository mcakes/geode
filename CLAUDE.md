# CLAUDE.md

Guidance for coding agents working in this repository.

## Read first

Geode is a keyboard-driven desktop shell for an index exotic equity
derivatives desk, written in Rust on GPUI and gpui-component with DuckDB as its
store.

Current documentation is authoritative:

- `docs/PHILOSOPHY.md` — product and engineering principles.
- `docs/current/architecture.md` — crate boundaries and runtime ownership.
- `docs/current/configuration.md` — layered documents, validation, writes, and
  reload.
- `docs/current/typed-documents.md` and `docs/current/configuration-dialogs.md` —
  typed readers, draft validation, overrides, and persistence.
- `docs/current/keymaps.md` and `docs/current/tiling.md` — binding resolution,
  layout, structural focus, and movement.
- `docs/current/data-path.md` — ingestion, storage, query, freshness, and
  health.
- `docs/current/shell.md` — tiles, input, focus, frame state, dialogs, and
  persistence.
- `docs/current/input-and-dialogs.md` — keyboard ownership, modal lifetime,
  palette, completion, choices, and frame pickers.
- `docs/current/features.md` — feature modules and their current limitations.
- Each crate README — local module map, commands, and narrow invariants.
- `docs/current/performance.md` — budgets, instrumentation, current reference
  values, and known gaps.
- `docs/current/real-sources.md` — where vendor clients plug in, what each
  simulator stands in for, and which feed shapes are guessed.
- `docs/agent-practices.md` — how work is run, reviewed, verified, and merged,
  and hazards that have cost time.
- `docs/open-items.md` — open follow-ups and owed display checks; remove an
  item when it is done.

`docs/phase-history.md` and `docs/superpowers/` are implementation archives.
They may describe superseded behavior and are not required reading. Consult
them only when a current guide omits the reason for an existing constraint.
Do not add task chronology or new "as built" sections to current guides.

## Commands

```sh
cargo run -p geode-app -- --demo [rows]
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
cargo run -p geode-collector -- --demo [ROWS]           # background collector loop
cargo run -p geode-collector -- install --dry-run       # print the login-job plan only
cargo run -p geode-collector -- status [--demo [ROWS]]  # who holds the store

bash scripts/mutation-check.sh "name substring"   # targeted mutation entries
bash scripts/mutation-check.sh --changed           # entries for changed files
bash scripts/mutation-check.sh --anchors-only      # validate anchors, filters and entries, no Cargo
bash scripts/mutation-check.sh --build-check "name substring"  # compile mutations, no tests
```

CI runs formatting, Clippy, tests, benchmark compilation, and the shell
`test-support` check on macOS and Windows. The macOS job first runs the
mutation checker's unit tests and the `--anchors-only` anchor and test-name
filter gate, before any toolchain step. The mutation harness runs under bash
or zsh, including Git Bash on Windows (not PowerShell); it needs Python 3 as
`python3` or `python`, or set `PYTHON`.

The repository does not require `sccache`; some machines configure it in the
user-level Cargo config. Where it is configured, Cargo needs access to its
local cache service; run it with approved permissions when the sandbox blocks
that connection. Cargo caches failed `rustc -vV` probes in the target
directory's `.rustc_info.json`, so a sandbox `Operation not permitted` can keep
breaking ordinary terminal builds. After that failure, remove only the
generated `.rustc_info.json` and rerun Cargo with cache-service access. Do not clean compiled artifacts or disable
the user's global cache configuration. For an intentional per-command Rust
wrapper bypass, use `RUSTC_WRAPPER= CARGO_CACHE_RUSTC_INFO=0`; native builds
may still use separately configured `CC`/`CXX` wrappers.

## Dependency and ownership rules

- `geode-core` is shared vocabulary and pure logic. Keep typed interpretation
  and merging free of I/O; its existing `Config::read_docs` and `Config::load`
  entry points read configuration files. It owns no socket, database, or window.
- `geode-shell` and `geode-data` never depend on each other or on feature
  modules.
- Feature modules do not depend on sibling features. They implement
  `TileContent` and carry their own `DataHandle` when needed.
- `geode-tile` sits between `geode-shell` and the feature modules. It depends
  on `geode-shell` and `geode-core`, never on `geode-data` or a feature
  module; `geode-shell` never depends on it.
- `geode-compose` is the gpui-free half of the composition root: the builtin
  data layer, `engine_setup`, the adapter registry (`adapters`), store and
  config paths, and the demo transports. It depends on `geode-core`,
  `geode-data`, `geode-documents` and `geode-demo-data`, never on gpui,
  `geode-shell`, `geode-tile` or a feature module (check:
  `cargo tree -p geode-compose -e normal | grep -c gpui` is 0).
- `geode-collector`, the headless background collector, depends on
  `geode-compose`, `geode-data` and `geode-core`, never on gpui or a UI
  crate, even as a dev-dependency (check:
  `cargo tree -p geode-collector -e normal | grep -c gpui` is 0, and the
  same with `-e dev`).
- `geode-collector install` registers a login agent (launchd or Task
  Scheduler). It is the only opt-in, and the app never starts a collector.
  The owner runs it himself: an agent never runs `install`, `launchctl` or
  `schtasks`, and uses `install --dry-run` and the pure plan builders.
- `geode-app` is the only composition root that adds UI: it builds on
  `geode-compose`, registers pricers, vol models, module factories and
  panels, and opens the window. The app and the collector must build equal
  schemas and sources; a test in `geode-app` holds them to it.
- Only `geode-data` owns source I/O and DuckDB connections. Only the ingest
  runner owns the writer.
- Calculation crates remain leaves behind request/outcome traits. UI modules
  do not call a pricing implementation directly.
- Every new library or binary target sets `bench = false`; Criterion targets
  set `harness = false`.

## UI and GPUI rules

- Use the `gpui-kit` skill for GPUI architecture, entities, focus, async work,
  components, and tests. Use `gpui-kit-design-guides` before changing a visual
  surface or interaction.
- Initialize `gpui_component` before component use and wrap the window's view
  in `gpui_component::Root`.
- Retained state belongs in the narrowest entity or model that owns it. Do not
  mutate state, perform I/O, or allocate unbounded work during render.
- Every pointer action has a keyboard route. `:` commands affect only their
  tile; application and frame changes use registered actions.
- Dialogs open through `shell::dialog::open_shell_dialog`. The pure dialog
  draft is the source of truth; `sync_dialog_text` is the text/focus bridge.
- A tile mechanism two modules would otherwise each write lives in
  `geode-tile` — interaction behavior (keys, focus, open and close,
  precedence) as well as paint: popups (`popover`), `.` action menus
  (`menu`), the in-tile y/n confirm (`confirm`), notices (`notice`) and the
  flip-barrier state machine (`following`).
- In object, settings, and keybinding dialog filters, Escape restores the entry
  query and bare Enter keeps the typed query; neither opens a row or commits.
  Use `dialogmode::{filter_exit, enter_filter, exit_filter}` for keyboard and
  pointer transitions. Value fields, naming, and settings choices own their keys.
- A surface dropping a focused input blurs it first. Tile focus movement arms
  the shell's focus restoration path.
- Repeated elements use stable domain-derived IDs. Theme tokens and the rem
  scale own application presentation; avoid literal colors, radii, and
  unexplained fixed pixels.
- The five module-visible GPUI globals are `UiSettings`, `Chords`, `AppClock`,
  `SeriesSettings`, and `ReferenceGlobal` (live reference tables).
  Add a global only for genuinely app-wide state that
  independently hosted modules must observe.

## Data and configuration rules

- The UI thread never waits for data work. Bounded submission returns a
  refusal the caller handles; it does not create a request that waits forever.
- Preserve source time, per-partition generations, grain-aware aggregation,
  and NULL-book behavior. Incorrect narrowing or plausible wrong totals are
  more serious than an explicit error.
- Health is keyed by source. Discovery and load are independent lanes; a clean
  poll cannot clear a degraded publish.
- Runtime config writes target only the user layer and go through
  `geode_shell::config_write`. Hot reload keeps the last valid state.
- TOML order is significant. Workspace-wide `preserve_order` must stay on.
- Existing DuckDB payload tables are not migrated automatically. After a demo
  schema change, delete `$TMPDIR/geode-demo/<rows>-42/` before running it.
- Any change to the store's DDL — its own tables (catalog, generations
  summary, provenance, `subscription_topics`, `geode_meta`) or the payload
  table builders in `store/ddl.rs` and `store/series.rs` (series payload and
  coverage) — bumps `geode_data::STORE_FORMAT`. Staging tables are exempt.
- Displayed times use `geode_core::clock::Clock`; do not use `chrono::Local`.

## Performance and tests

- Pure UI work targets 8 ms; a one-million-row requery targets 50 ms. Measure
  changes to named hot paths and record conditions in the measurement log.
- Keep prepared models outside render, virtualize large collections, and avoid
  per-frame formatting or heap churn.
- Test at the lowest layer that proves the behavior: pure state first, then
  GPUI context/window tests for focus and interaction, then a real-window
  display check for visual facts unavailable headlessly.
- Test production routes. Calling an internal mutation does not prove that a
  key, pointer event, delivery, or focus transition reaches it.
- The mutation harness verifies that a named test detects a specific broken
  behavior. Add targeted entries for changed correctness contracts. Commit or
  otherwise preserve work before running mutations because the harness edits
  tracked files in place while testing and restores them afterward.
- Run `--anchors-only` before merge. Use targeted names or `--changed` during
  development; a full 1,000-plus-entry run is an audit, not an everyday loop.

## Workspace dependencies

All gpui and gpui-component family crates are exact-version pinned in the root
`Cargo.toml`. Bump the two families together deliberately. The registry source
for the pinned versions is the API authority.

Release and benchmark profiles retain debug symbols for profiling.

## Documentation maintenance

Describe what the system does, why the constraint exists, its failure
semantics, and known limitations. Keep implementation sequences, review rounds,
and superseded alternatives in the archive. A code comment should state the
local invariant and failure it prevents; it should not require a task number or
spec section to make sense.

When behavior changes, update the relevant current guide and crate README in
the same change.
