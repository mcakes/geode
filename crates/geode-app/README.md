# geode-app

The `geode` binary. This is the one crate where everything meets: it
loads the layered config, builds the action registry and keymap, installs
logging, spawns the data service, fills the module roster and opens the
window on `geode_shell::shell::ShellView`.

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
| Builtin | compiled in (`geode_shell::defaults`, plus `builtin_layer`'s keymap, the pricer's bundled views and templates, its two datasets: `pricer_sheets` and the computed `pricer`, and the builtin market-data panels) |
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

Document producers run asynchronously through the normal subscription path;
data need not be available in the first frame. Demo series use fixed weekday
sessions from 14:30 to 21:00 UTC, without holiday or daylight-saving rules.
`demo_kdb` advertises identities; `demo_rest` exercises manual identity entry.

`demo_positions` carries a Move LHU by rewriting the risk CSVs in the source
directory, so a move persists across `--demo` launches until the directory is
deleted. A move to another book's LHU leaves the position's `Book` unchanged.

`demo_refdb` answers the `refdb` snapshot source every 30 s with the ten demo
underlyings. Every third poll renames one row, so two in three polls are
skipped as unchanged and the third publishes a new generation. The poll count
lives in the process, so each launch starts again from revision 0. Its
`calendar` column is an exchange calendar code; no holiday dates are modelled.

## What lives here

| Module | Holds |
|---|---|
| `main` | Startup composition: logging, config, registry and keymap, roster (module factories, then the row menu's actions: `add_dimension_actions` registers `geode-nemo`'s two Open in Nemo actions, then `geode-positions`' Move LHU, disabled unless startup resolved a position service), service, and window. Pure argument parsing and user-path resolution; `config_dirs` reads the environment. |
| `bridge` | Service setup and module factories (loads `panels` against the registered document kinds and kind actions and builds one market-data factory per accepted panel; refused panels, including one named after another module's kind, become composition diagnostics in the shell's config section), window event routing (including stopped data threads, and the handle's `Busy`-refusal total read on each drained event into `Diagnostics`), catalog refresh/retry (a `Stopped` refusal drops the demand), the startup schema's reference datasets handed to `Diagnostics` at attach, reference reads at the frame's as-of and poll-now requests drained from `Diagnostics` (only the latest-tagged `DataEvent::Reference` is stored; a refusal is stored with its reason, never retried), position-service resolution from `positions.toml` (`positions_configured`), each `DataEvent::Command` routed to `ShellView::note_command`, and forwarding view reloads to the data service (a refused hand-off is a diagnostic). |
| `events` | Coalesced pending state with a one-slot wakeup channel. Retains publication book unions and highest-tagged query results; upload outcomes have separate `(tile key, tag)` entries; local-write and position-command outcomes never coalesce. |
| `demo` | `--demo`: the temp directory, the emitted sources, the compiled-in demo config layer (including `positions.toml`), and `DemoPositions` (`demo_positions`), the demo position system: a Move LHU rewrites the `LHU` field in the same risk CSV, then its sentinel with a strictly later `as_of`, after refusing any unknown position before writing. `Book` is not rewritten. |
| `demo_bus` | Demo-only CVI, dividend and option-chain producers publishing through `ChannelAdapter` and the normal document writers/parsers. The startup burst publishes each key `startup_repeats` times, so the chain producer, which rotates through one expiry per publish, sends every expiry of every underlying before the first cadence wait. A producer's `next` returns `None` to skip a publish: the chain producer prices off the latest CVI document the CVI producer stored for that underlying (`cvi_next`/`chain_next`) and skips until there is one. The same adapter accepts configured uploads, whose bus messages follow subscription ingestion. |
| `demo_refdb` | Demo mode's reference database (`DemoRefDb`, `demo_refdb`): the `underlyings` table with hand-written vendor tickers, currencies, calendars, exchanges and multipliers. Snapshot side only; `fail_next` and the query delay exercise the degraded and slow paths in tests. |
| `demo_series` | Demo mode's fetch adapter: seeded, span-independent one-minute bars for two dozen identities, behind two sources (`demo_kdb` with a catalogue, `demo_rest` without). |
| `crash` | Log-file trimming at startup and the process panic hook: marked containment boundaries log without a report; other panics attempt a report before chaining the previous hook. |
| `assets` | The asset source: gpui-kit's component icons plus the catalogue icons Geode's own surfaces name. |

## Dependencies

This composition root imports the shell, data service, feature modules,
concrete document parsers, and pricing implementation. `geode-shell` and
`geode-data` never depend on each other; feature crates do not depend on sibling
features. Registering `geode-documents` here keeps parser dependencies out of
the data service.

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
  can wait on one exact outcome with no timeout. `pin_app_datasets` keeps
  the builtin declaration of `pricer_sheets` and of the computed `pricer`
  against a differing layer redeclaration, at startup and on reload, with
  an error diagnostic.
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

Upload targets resolve against registered adapters at startup. The bridge passes
target/document lists to market-data factories and routes outcomes to the
submitting tile, including hidden occupants. Each upload tag retains its own
mailbox entry. A closed tile cannot receive its outcome; data-tier logging
still records completed transport calls and normal refusals. Egress edits mark
restart required and do not replace running transports or panel target lists.
