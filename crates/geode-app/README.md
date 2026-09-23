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
cargo run -p geode-app -- --demo [rows]       # generated data, no real source (default 100,000 rows)
cargo run -p geode-app --features profiling   # gpui's profiler and the perf::* actions
```

Config directories:

| Layer | Where |
|---|---|
| Builtin | compiled in (`geode_shell::defaults`) |
| Desk | `$GEODE_DESK_CONFIG`, if set |
| User | `%APPDATA%\geode` on Windows, else `$HOME/.config/geode` |

`--demo` layers `examples/demo-config` under any desk and user config,
emits the generator's source directory once per row count, and points the
database at `$TMPDIR/geode-demo/<rows>-42/geode.duckdb`. Delete that
whole directory after any change to the demo schema's columns or to the
demo series walk; nothing migrates an existing database.

## What lives here

| Module | Holds |
|---|---|
| `main` | Startup order: logging, config, registry and keymap, roster, service, window. `parse_args` and `config_dirs` are pure and tested. |
| `bridge` | Service setup and module factories, window event routing, catalog refresh/retry, and forwarding view reloads to the data service. |
| `events` | Coalesced pending outcomes and state with a one-slot wakeup channel. Retains publication book unions and highest-tagged results. |
| `demo` | `--demo`: the temp directory, the emitted sources, the compiled-in demo config layer. |
| `demo_bus` | Demo mode's producer for the market-data path: a thread generating CVI documents and publishing them onto a `ChannelAdapter` through the same wire format a real subscribed source's receiver parses. Registered only under `--demo`. |
| `demo_series` | Demo mode's fetch adapter: seeded, span-independent one-minute bars for two dozen identities, behind two sources (`demo_kdb` with a catalogue, `demo_rest` without). |
| `crash` | Log-file trimming at startup and the process panic hook that tells a contained panic from a fatal one and writes a crash file. |
| `assets` | The asset source: gpui-kit's component icons plus the catalogue icons Geode's own surfaces name. |

## Dependencies

This crate depends on every other workspace crate, and is the only one
allowed to. `geode-shell` and `geode-data` never depend on each other; a
module crate depends on both but never on another module. Document kinds
(`geode-documents`) are registered here, which is why the data service
never names a parser crate.

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
- The daily log file and crash file names roll on the UTC date; every
  displayed time is the trader's local clock.
- `NamedColours::from_doc` diagnostics are reported only by the bridge's
  `data_setup` and `ConfigReloaded` arms.
