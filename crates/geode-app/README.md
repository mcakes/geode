# geode-app

The `geode` binary. This is the one crate where everything meets: it
loads the layered config, builds the action registry and keymap, installs
logging, spawns the data service, fills the module roster, builds the one
`geode_shell::shell::ShellApp` and opens its windows, each a
`geode_shell::shell::ShellView` over it.

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

On Windows, Velopack handles installer callbacks before logging, argument
parsing, configuration, or GPUI startup. The SDK is pinned to the packaging
tool version and is a Windows-only dependency. Applying pending updates on
startup is explicitly disabled until collector shutdown and restart are
integrated. There is no update feed polling yet. See
[Windows packaging](../../docs/current/packaging.md) for producing a per-user
installer and staging vendor DLLs.

Config directories:

| Layer | Where |
|---|---|
| Builtin | compiled in (`geode_shell::defaults`, plus `builtin_layer`'s keymap, the pricer's bundled views and templates, the builtin market-data panels and the builtin scenario shapes, then `geode_compose::builtin_data_layer`: the two app datasets and the `--demo` layer) |
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
| `main` | Startup composition: logging (`geode_compose::logging::install("geode", true)`), config, registry and keymap, roster (module factories, then the row menu's actions: `add_dimension_actions` registers `geode-nemo`'s two Open in Nemo actions, then `geode-positions`' Move LHU, disabled unless startup resolved a position service), service, the window opener (`bridge::window_opener`, installed through `ShellApp::set_window_opener`) and the startup windows (`startup::open_startup_windows`). Pure argument parsing; store and config paths come from `geode_compose` (`store_path`, `config_dirs`). |
| `bridge` | Service setup and module factories (`attach` holds the `ShellApp` entity and no window: it runs once per process, ahead of the first window, routes each data answer through `ShellApp::deliver` when it arrives (a keyed answer to the window hosting its tile, a keyless or shell-keyed one to every window, one for a parked tile dropped), checks the app's registry (`any_window_open`) for liveness on every event and in every retry timer, and reads the catalog and reference lanes' as-of from the window whose diagnostics page is open, else the oldest window, else the shared lane; `window_opener` is the one opener every window opens through, in production over `TitleBar::window_options` and in the tests over default options: it places the window (`placement::options_for`, over the spawning window's frame and display carried in `Placement::Beside` by the verb, else read through its handle, the attached displays and the primary; a spawner that cannot be read leaves placement to the platform with a debug line), opens a `Root` over a `ShellView` for the reserved key and calls `attach_window`; `attach_window` installs a window's own should-close hook from the opener, with or without a bridge: a close that leaves another window open deregisters the window and parks its tiles, the last window's close saves the session and unsaved sheets first; `attach` also subscribes to `ShellApp`, the one emitter of `ShellEvent`, for reloads and distinct requests, and reads the reloaded config from it; loads `panels` against the registered document kinds and kind actions and builds one market-data factory per accepted panel; refused panels, including one named after another module's kind, become composition diagnostics in the shell's config section; then `load_scenarios_from_config` loads `scenarios` against the schema after the panels, refuses a shape named like another module's or an accepted panel's kind, or whose `scenario:<kind>` view is already configured, withholds a builtin shape over an undeclared dataset with an info log line instead of an Error, hands the service the configured views then each accepted shape's derived view (never the blotter's view list), and builds one `ScenarioFactory` per shape after the panels, the first shipping the keymap fragment; the scenario factories share one schema, dimensions and colors cell that `ConfigReloaded` rewrites, and a reloaded view named like a running shape's derived view is dropped from every consumer with one Warning; `scenarios` is restart-required), window event routing (including stopped data threads, and the handle's `Busy`-refusal total read on each drained event into `Diagnostics`), catalog refresh/retry (a `Stopped` refusal drops the demand), the startup schema's reference datasets handed to `Diagnostics` at attach, reference reads at the frame's as-of and poll-now requests drained from `Diagnostics` (only the latest-tagged `DataEvent::Reference` is stored; a refusal is stored with its reason, never retried), the live reference cache behind `ReferenceGlobal` (every reference dataset read at `AsOf::Live` under `REFERENCE_KEY` at attach and on each of its publishes; only a dataset's latest tag is applied, the global is set only when a table changed, `Ok(None)` removes the table, a failed read keeps it and warns once per run of failures on `geode::reference`, a `Busy` refusal rereads after one second with one timer per dataset, `Stopped` drops the demand), position-service resolution from `positions.toml` (`positions_configured`), each `DataEvent::Command` routed to `ShellView::note_command`, and forwarding view reloads to the data service (a refused hand-off is a diagnostic). |
| `events` | Coalesced pending state with a one-slot wakeup channel. Retains publication book unions and highest-tagged query results; upload outcomes have separate `(tile key, tag)` entries; local-write, position-command, and reference-table outcomes never coalesce. |
| `watchlists` | The resolved watchlists behind `WatchlistGlobal`: `fold_watchlists` folds every list in the config against the schema the service serves (a rule that does not fold is a warning diagnostic and a `RuleError` on its list; the list is kept), and `WatchlistCache` resolves each list through `DataHandle::watchlist` under its own key from the `WATCHLIST_KEY_BASE` range, allocated on first sight of a name and never reused within the run. Only a list's latest tag is applied, and a per-list tag floor drops an answer to a definition since replaced or removed, even while the replacement's refresh waits on a `Busy` retry (one timer per list, one second); `Stopped` drops the demand. A failed resolution keeps the last members, marks the list `Failed` and warns once per run of failures on `geode::watchlist`; a successful one sets `Current`, `resolved_at` and the rule errors (the fold's plus the data layer's `rules_failed`). The bridge calls `set_definitions` at attach and on `ConfigReloaded`, `on_published` for a non-local publish, and `answer` for each `DataEvent::Watchlist` under a watchlist key; a retry timer submits only while the app's registry lists a window. |
| `placement` | Pure: `options_for` maps a `Placement` to the platform's `display_id` and `window_bounds` from the facts the opener gathers. `Beside`: the spawner's display and frame shifted by `CASCADE` (28 logical pixels) on both axes, clamped inside that display (shifted back by the overflow; a window larger than the display sits at its origin); no spawner asks for nothing. `Restored`: the recorded display and frame, `Maximized` when recorded so; a display no longer attached falls back to the primary with the recorded size at the primary's origin plus `CASCADE`; no recorded display leaves the frame alone. `Default`: nothing. Global logical coordinates, so a negative display origin needs no special case. |
| `startup` | `open_startup_windows`: one window per restored `[windows.K]` record through `ShellApp::open_window`, in K order (the first record's window is the oldest), each on its recorded index with a `Restored` placement; a refused open is warned and skipped; with none opened, one window on the session's initial index at the platform's default placement, and a refusal there ends the run. Its tests live beside the opener's in `bridge`, which hold the service fixtures. |
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

- Windows installer callbacks exit without creating user configuration, logs,
  a store, or a window. `tests/velopack_startup.rs` invokes the real executable
  with each lifecycle callback; it never runs an installer or registers a task.
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
- Every window opens through the one opener `main` installs on `ShellApp`
  (`bridge::window_opener` through `set_window_opener`, before the first
  window): it builds the platform window from `TitleBar::window_options`
  and the spec's `Placement` (`placement::options_for`: beside the window
  that asked, cascaded and clamped into its display; where a session
  recorded it, on the primary when that display is gone; or wherever the
  platform puts it), opens a `Root` over a `ShellView` for the reserved
  key, and installs the window's hooks (`bridge::attach_window`). The
  bridge's test opener is the same closure over default options, so the
  close and placement tests run the production route. Startup
  (`startup::open_startup_windows`) opens one window per restored record
  in K order, else one on the session's initial index; a refusal there
  ends the run, since without a window there is nothing to run. Nothing
  opens a window by hand. The spawning window's frame and display travel
  in `Placement::Beside`, read by the verb from its own window (a window
  under update cannot be read through its handle); the opener reads them
  through the handle only when they are absent, as an app-level caller
  leaves them.
- Closing the last window quits the app on every platform: `main` sets
  `QuitMode::LastWindowClosed` (gpui's default keeps a macOS process alive
  with no window). The should-close hook `attach_window` installs decides
  what a close means by `ShellApp::is_last_window`: with another window
  open it deregisters the window while it still stands
  (`ShellApp::window_closed`, which parks its tiles through the view with
  a live context) and saves nothing; for the last window it saves the
  session (parked records included), flushes unsaved pricer sheets, and
  only then deregisters, because `ShellApp::save_session` writes nothing
  once no window is registered. Either way the close proceeds. The quit
  hook saves once, from the app, and walks no windows: on an explicit quit
  every window is still registered; after the last close it finds none and
  writes nothing. The real close button is not exercised headlessly; tests
  drive the installed hook through `bridge::run_should_close_for_test`,
  which is `VisualTestContext::simulate_close` (it runs the one handler
  the test platform window holds, the production closure, and does not
  remove the window). Also untestable headlessly: the quit-mode line (gpui
  exposes no reader and the test platform's quit does nothing), and the
  order "the close removes the window, then the quit hook finds none".
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
