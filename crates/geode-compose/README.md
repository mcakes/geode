# geode-compose

The gpui-free half of Geode's composition root: the part of composition a
headless process can use. The app and the background collector build their
store configuration from these functions, so the two agree on what the store
holds.

It holds the builtin documents that decide the store (`builtin_data_layer`:
the app's `pricer_sheets` and `pricer` declarations, which live in
`geode_core::builtin`, plus the `--demo` layer), `engine_setup`, the store
and config paths, logging setup, the collector's settings, and the demo
transports. It never depends on gpui. The
app's builtin layer is its own documents plus the data layer, so a process
built from the data layer alone and the same desk and user directories has
the app's schema and sources; `geode-app` tests that contract.

Current crate boundaries:
[`docs/current/architecture.md`](../../docs/current/architecture.md).

## What lives here

| Module | Holds |
|---|---|
| `lib.rs` | `builtin_data_layer`: the two app datasets and, with a demo directory, the `--demo` layer. `engine_setup`/`EngineSetup`: the engine's half of `DataServiceConfig` (schema, sources, dimensions, document kinds, clock, adapters; empty views, no pricer, vol, egress or positions), infallible, with an empty schema when there is no `datasets` doc. `pin_app_datasets`: keeps `pricer_sheets` and `pricer` as builtin declares them against a differing layer redeclaration, with an error diagnostic; the app's reload path calls it too. `adapters`: the transport registry, empty without a demo directory, otherwise the five demo transports and the bus's feed. `load_config`: the collector's configuration, `builtin_data_layer` alone under the given desk and user directories. `store_path`: `db_path` with `LOCALAPPDATA` and `HOME` read from the environment; the app and the collector both call it. |
| `logging` | `install(prefix, stderr)`: process-wide tracing with a reloadable level filter, a stderr layer when `stderr` is true (the collector under a service manager passes false), a ring layer, and a daily `<user>/logs/<prefix>.YYYY-MM-DD.log` file (UTC dates) behind a non-blocking writer, returned as `Logging { ring, control, guard }`. The app passes `geode`, the collector `collector`. `trim_log_files(dir, prefix, keep)` caps one prefix's files at startup (seven), leaving the other process's files in the shared directory alone; `prune_files` is the shared keep-the-last-N-names helper the app's crash reports also use. |
| `collector` | `collector_settings`/`CollectorSettings`: `[collector] memory_limit` from `app.toml`, an opt-in: unset by default, so the collector leaves DuckDB's own default, as the app does. A finite 512MB limit made DuckDB abort the collector with an internal assertion on large CSV loads (`docs/perf.md`, 2026-10-04); a value can be set once the overnight footprint measurement chooses one. A value that is not a number and a DuckDB size unit (`B`…`TB`, `KiB`…`TiB`, any case) warns at `app.collector.memory_limit` and sets no limit. |
| `paths` | Where the store and the configuration layers live on disk: `db_path` (`data.db_path`, then the demo directory, then the platform directory), `config_dirs` (reads `GEODE_DESK_CONFIG`, `APPDATA`, `HOME`) and its pure core `user_config_dir`. |
| `demo` | Generated inputs and builtin configuration for `--demo`: the temp directory (`demo_dir`), the emitted sources (`ensure_emitted`), the compiled-in demo config layer (`layer`, including `positions.toml`), and `DemoPositions` (`demo_positions`), the demo position system. |
| `demo_bus` | Background document publishing for `--demo`: CVI, dividend and option-chain producers publishing through `ChannelAdapter` and the normal document writers and parsers. `spawn_default` is the bus both binaries run: the three producers over the demo underlyings, a 5 s cadence, 2 s jitter, seed 42. |
| `demo_series` | Deterministic one-minute bars for the demo fetch sources `demo_kdb` (with a catalogue) and `demo_rest` (without). |
| `demo_refdb` | Demo mode's reference database (`DemoRefDb`, `demo_refdb`): the `underlyings` table behind the `refdb` snapshot source. Snapshot side only. |

Tests that need the app's bridge or a window live in
`geode-app/src/demo_tests.rs`.

## Demo behavior

`DemoPositions` carries a Move LHU by rewriting the `LHU` field in the same
risk CSV in the source directory, then its sentinel with a strictly later
`as_of`, after refusing any unknown position before writing. A move persists
across `--demo` launches until the directory is deleted. A move to another
book's LHU leaves the position's `Book` unchanged.

The demo bus's startup burst publishes each key `startup_repeats` times, so
the chain producer, which rotates through one expiry per publish, sends every
expiry of every underlying before the first cadence wait. A producer's `next`
returns `None` to skip a publish: the chain producer prices off the latest
CVI document the CVI producer stored for that underlying (`cvi_next`,
`chain_next`) and skips until there is one. The same adapter accepts
configured uploads, whose bus messages follow subscription ingestion.

Document producers run asynchronously through the normal subscription path;
data need not be available in the first frame. Demo series use fixed weekday
sessions from 14:30 to 21:00 UTC, without holiday or daylight-saving rules.
`demo_kdb` advertises identities; `demo_rest` exercises manual identity entry.

`demo_refdb` answers the `refdb` snapshot source every 30 s with the ten demo
underlyings, with hand-written vendor tickers, currencies, calendars,
exchanges and multipliers. Every third poll renames one row, so two in three
polls are skipped as unchanged and the third publishes a new generation. The
poll count lives in the process, so each launch starts again from revision 0.
Its `calendar` column is an exchange calendar code; no holiday dates are
modelled. `fail_next` and the query delay exercise the degraded and slow
paths in tests.

## Rules this crate pins

- No gpui, `geode-shell`, `geode-tile`, `geode-widgets` or feature module,
  not even as a dev-dependency. Both checks print 0:

  ```sh
  cargo tree -p geode-compose -e normal | grep -c gpui
  cargo tree -p geode-compose -e dev | grep -c gpui
  ```

- `builtin_data_layer` contributes `pricer_sheets`, then `pricer`, then the
  demo layer. Dataset order is part of the schema the app's
  equal-configuration test compares, so the app's builtin layer appends
  this layer unchanged.

## Commands

```sh
cargo test -p geode-compose
```
