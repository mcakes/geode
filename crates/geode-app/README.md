# geode-app

The `geode` binary. This is the one crate where everything meets: it
loads the layered config, builds the action registry and keymap, installs
logging, spawns the data service, fills the module roster and opens the
window on `geode_shell::shell::ShellView`.

The `guide` tile factory is always registered, independently of data setup.
It displays the bundled user guide through **Guide: Split** in the palette
or **guide** in the tile picker. Its kind is reserved against market-data
panel declarations, as are the other feature kinds.

Current crate boundaries and runtime ownership:
[`docs/current/architecture.md`](../../docs/current/architecture.md).
Feature composition and demo behavior are described in
[`docs/current/features.md`](../../docs/current/features.md#demo-and-application-composition).

## Running

```sh
cargo run -p geode-app                        # real sources from config
cargo run -p geode-app -- --demo [rows]       # demo defaults (default 100,000 rows)
cargo run -p geode-app --features profiling   # gpui's profiler and the perf::* actions
```

Config directories:

| Layer | Where |
|---|---|
| Builtin | compiled in (`geode_shell::defaults`, plus `builtin_layer`'s keymap, the pricer's bundled views and templates and the builtin market-data panels, then `geode_compose::builtin_data_layer`: the two app datasets and the `--demo` layer) |
| Desk | `$GEODE_DESK_CONFIG`, if set |
| User | `$APPDATA/geode` when set, otherwise `$HOME/.config/geode` |

Directory resolution reads environment values without checking existence or
writability. Without `APPDATA` or `HOME`, user configuration, session storage,
and file logging have no resolved user path.

`--demo` layers `examples/demo-config` under desk and user overrides, which
can still change sources. It emits generated files when the source directory
is empty and uses `$TMPDIR/geode-demo/<rows>-42/geode.duckdb` by default.
Any existing source-directory entry suppresses generation; reuse does not
validate completeness. Delete the whole directory after changes to demo schema
columns or series generation, since existing tables and cached values are
not migrated.

The demo transports, their behavior and the store and config paths live in
[`geode-compose`](../geode-compose/README.md).

## What lives here

| Module | Holds |
|---|---|
| `main` | Startup composition: logging (`geode_compose::logging::install("geode", true)`), config, registry and keymap, roster (module factories, then the row menu's actions: `add_dimension_actions` registers `geode-nemo`'s two Open in Nemo actions, then `geode-positions`' Move LHU, disabled unless startup resolved a position service), service, and window. Pure argument parsing; store and config paths come from `geode_compose` (`store_path`, `config_dirs`). |
| `bridge` | Service setup and module factories (loads `panels` against the registered document kinds and kind actions and builds one market-data factory per accepted panel; refused panels, including one named after another module's kind, become composition diagnostics in the shell's config section), window event routing (including stopped data threads, and the handle's `Busy`-refusal total read on each drained event into `Diagnostics`), catalog refresh/retry (a `Stopped` refusal drops the demand), the startup schema's reference datasets handed to `Diagnostics` at attach, reference reads at the frame's as-of and poll-now requests drained from `Diagnostics` (only the latest-tagged `DataEvent::Reference` is stored; a refusal is stored with its reason, never retried), the live reference cache behind `ReferenceGlobal` (every reference dataset read at `AsOf::Live` under `REFERENCE_KEY` at attach and on each of its publishes; only a dataset's latest tag is applied, the global is set only when a table changed, `Ok(None)` removes the table, a failed read keeps it and warns once per run of failures on `geode::reference`, a `Busy` refusal rereads after one second with one timer per dataset, `Stopped` drops the demand), position-service resolution from `positions.toml` (`positions_configured`), each `DataEvent::Command` routed to `ShellView::note_command`, and forwarding view reloads to the data service (a refused hand-off is a diagnostic). |
| `events` | Coalesced pending state with a one-slot wakeup channel. Retains publication book unions and highest-tagged query results; upload outcomes have separate `(tile key, tag)` entries; local-write, position-command, and reference-table outcomes never coalesce. |
| `demo_tests` | Tests of `geode_compose`'s demo modules that need the bridge, `builtin_layer`, a feature module or a window. |
| `crash` | The process panic hook and crash-report retention (through `geode_compose::logging::prune_files`): marked containment boundaries log without a report; other panics attempt a report before chaining the previous hook. |
| `assets` | The asset source: gpui-kit's component icons plus the catalogue icons Geode's own surfaces name. |

## Dependencies

This composition root imports the shell, data service, `geode-compose`,
feature modules, and pricing implementation. `geode-shell` and `geode-data`
never depend on each other; feature crates do not depend on sibling features.
The concrete document parsers are registered through `geode-compose`
(`engine_setup`), which keeps parser dependencies out of the data service;
`geode-documents` is only a dev-dependency here, for the demo tests.

`gpui-base` and `gpui-component-macros` are listed as direct dependencies
without being imported: gpui-component names them with a caret, and the
direct `=` pin here keeps the whole gpui-kit family in step under
`cargo update`. See the root `Cargo.toml`'s dependency comment.

## Commands

```sh
cargo test -p geode-app
cargo check -p geode-app --features profiling
```

## Rules this crate pins

- `gpui_component::init(cx)` runs before any component use, and the root
  view is wrapped in `gpui_component::Root`.
- `ShellServices.keymap_diagnostics` and `keymap_fragment_diagnostics`
  ride from here into the diagnostics config section because they cannot
  be recomputed from the config alone.
- The bridge awaits a state mailbox on the foreground executor. Only its
  wakeup channel has a fixed capacity; pending state coalesces by recipient
  or source without an overall key-count cap. See
  [requests and UI delivery](../../docs/current/request-delivery.md).
- Daily logs use UTC dates under `<user>/logs`; panic reports use UTC
  timestamps under `<user>`. Reports read the synchronous log ring, so they
  do not depend on buffered daily logs being flushed. The containment marker
  controls report creation; its absence does not prove the process will exit.
- Keep the logging worker guard until exit and drop it before `process::exit`
  so buffered startup diagnostics are flushed. File setup failures leave
  stderr and ring logging available.
- `NamedColours::from_doc` diagnostics are reported only by the bridge's
  `data_setup` and `ConfigReloaded` arms.
- Every local-write outcome for `pricer_sheets` reaches the pricer factory
  (`save_answered`/`forget_answered`), in the writer's order: a pricer tile
  can wait on one exact outcome with no timeout.
  `geode_compose::pin_app_datasets` keeps the builtin declaration of
  `pricer_sheets` and of the computed `pricer` against a differing layer
  redeclaration, at startup and on reload, with an error diagnostic.
- The app and the collector build equal schemas and sources from the same
  desk and user directories:
  `the_app_and_the_collector_build_the_same_schema_and_sources` compares the
  app's `builtin_layer` through `bridge::data_setup` with
  `geode_compose::builtin_data_layer` through `engine_setup`.
- Closing the window quits the app on every platform: `main` sets
  `QuitMode::LastWindowClosed` (gpui's default keeps a macOS process alive
  with no window). `save_on_close` registers the main window's should-close
  hook, which saves the session and flushes unsaved pricer sheets while the
  window still exists, then lets the close proceed: gpui removes the window
  before the quit hooks run, so the session quit hook would find none. The
  quit hooks run after it too: the session hook finds no window and saves
  nothing, and the sheet flush finds nothing dirty. The real close button is
  not exercised headlessly; tests reach the hook through
  `VisualTestContext::simulate_close`, which runs only the handler. Also
  untestable headlessly: the quit-mode line (gpui exposes no reader and the
  test platform's quit does nothing), and the order "the close removes the
  window, then the quit hook finds none".
- At quit, `stop_at_quit` attempts each pricer tile's unsaved-sheet flush
  before starting data shutdown on a background executor. Admitted writes
  precede `Shutdown` and the writer drains them, but submission/write failures
  and GPUI's 200 ms quit deadline can still leave sheets unsaved. The demo bus
  also stops in a background quit hook after any in-progress generation or
  publication completes.
- Test fixtures hosting a pricer tile install `geode_pricer::init` after
  `gpui_component::init`, as `main` does: gpui gives the later binding
  precedence, and the reverse order lets `DataTable`'s own keys beat the
  tile's.
- The pricer hears every config reload through the frame's `config` counter
  but reloads only when `pricer_config_key` changed from the last applied key
  (seeded with the startup key `start` carries on `Bridge`), so an unrelated reload
  neither restarts its tiles' refresh timers nor repeats a bad value's warning.
- `[pricing] underlyings` backs the pricer's `UnderlyingSource` through the
  one `UnderlyingList` on `Bridge`; `start` hands it to the factory and the
  reload observer sets it, so the pricer only ever reads it.
- `[pricing] payout_currency` resolves to a `PayoutSource` in
  `PricerSettings` at startup and on each pricer reload, both against the
  startup schema `Bridge` keeps: datasets are restart-required, so an edited
  `datasets` doc awaiting restart must not decide which column is read.
  An invalid value resolves to `None` on a reload too, so it drops the
  running source rather than keeping the last good one (`underlyings`
  keeps its list); the error diagnostic names it.

Upload targets resolve against registered adapters at startup. The bridge passes
target/document lists to market-data factories and routes outcomes to the
submitting tile, including hidden occupants. Each upload tag retains its own
mailbox entry. A closed tile cannot receive its outcome; data-tier logging
still records completed transport calls and normal refusals. Egress edits mark
restart required and do not replace running transports or panel target lists.
