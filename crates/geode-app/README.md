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
| Builtin | compiled in (`geode_shell::defaults`, plus `builtin_layer`'s keymap, the pricer's bundled views and templates, and its `pricer_sheets` dataset) |
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

## What lives here

| Module | Holds |
|---|---|
| `main` | Startup composition: logging, config, registry and keymap, roster, service, and window. Pure argument parsing and user-path resolution; `config_dirs` reads the environment. |
| `bridge` | Service setup and module factories, window event routing, catalog refresh/retry, and forwarding view reloads to the data service. |
| `events` | Coalesced pending state with a one-slot wakeup channel. Retains publication book unions and highest-tagged query results; upload outcomes have separate `(tile key, tag)` entries; local-write outcomes never coalesce. |
| `demo` | `--demo`: the temp directory, the emitted sources, the compiled-in demo config layer. |
| `demo_bus` | Demo-only CVI and dividend producers publishing through `ChannelAdapter` and the normal document writers/parsers. The same adapter accepts configured uploads, whose bus messages follow subscription ingestion. |
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
  can wait on one exact outcome with no timeout. `pin_pricer_sheets` keeps
  the builtin declaration against a differing layer redeclaration, at
  startup and on reload, with an error diagnostic.
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
