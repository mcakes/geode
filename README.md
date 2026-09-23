# Geode

Geode is the everything-tool for an index exotic equity derivatives desk:
risk, pricing, execution and data visualisation in one permanent,
keyboard-driven shell. It is written in Rust on gpui (Zed's UI framework)
and gpui-component, with DuckDB as the store.

Three principles from the charter (`docs/PHILOSOPHY.md`) shape everything
in this repository:

- **A lens, not a brain.** All financial computation lives upstream. Geode
  only shapes views: grouping, filtering, aggregating and joining data
  that other systems produced.
- **The keyboard is the interface.** i3-style tiling, vim-style modal
  navigation inside tiles, and a command palette as the discoverable
  fallback. Every mouse action has a keyboard twin.
- **Latency is a feature, silence is a bug.** Nothing the data layer does
  may stall the render thread, and when data is slow or degraded the UI
  says so honestly.

## Running it

```sh
cargo run -p geode-app -- --demo          # generated data, no real source (100,000 rows)
cargo run -p geode-app -- --demo 1000000  # a larger generated book
cargo run -p geode-app                    # real sources from config
```

The toolchain is stable Rust (`rust-toolchain.toml`). On macOS no Xcode
install is needed: Metal shaders compile at runtime.

Demo mode layers `examples/demo-config` under any desk and user config,
emits a deterministic source directory and database under
`$TMPDIR/geode-demo/<rows>-42/`, and runs an in-process market-data bus
and two fetch sources so every module has data. Delete that directory
after changing the demo schema; nothing migrates an existing database.

Configuration is three TOML layers, deep-merged with provenance:

| Layer | Where |
|---|---|
| Builtin | compiled in |
| Desk | `$GEODE_DESK_CONFIG` |
| User | `%APPDATA%\geode` on Windows, `$HOME/.config/geode` elsewhere |

Every setting the dialogs write lands in the user layer, and the app
hot-reloads a changed file, keeping the last good config if the new one
is rejected.

## Finding your way around

`mod` is `alt` by default and can be set to `cmd` in `app.toml`; `ctrl`
is reserved for the shipped literal bindings.

| Keys | Does |
|---|---|
| `ctrl+k` | command palette, the universal fallback |
| `mod+h/j/k/l`, `mod+1..9` | move focus between tiles, switch workspace |
| `mod+n`, `mod+f`, `mod+e` | add a tile, fullscreen, flip a split |
| `mod+[`, `mod+]` | cycle a tile stack |
| `ctrl+1..9`, `ctrl+0` | grouping slots, clear |
| `mod+g`, `mod+p`, `mod+t` | grouping picker, dimension picker, as-of |
| `mod+/`, `mod+z`, `mod+shift+z` | scope text, scope undo, redo |
| `/`, `:` in a tile | find, the tile's command line |
| `mod+shift+p` | frame-time overlay |

The which-key strip shows what a pending chord can continue with, and the
keybindings dialog (from the palette) rebinds anything.

## Repository layout

```
crates/
  geode-core         shared vocabulary: config, schema, scope, snapshot, colour, log
  geode-data         DataService: sources, ingest, DuckDB store, query path, health
  geode-shell        tiling WM, keymap engine, palette, frame, dialogs, module contract
  geode-blotter      any view as a collapsible keyboard-driven hierarchy
  geode-marketdata   market-data document panels with an edit draft (CVI)
  geode-diagnostics  the diagnostics tile over health, generations, config and the log
  geode-documents    typed parsers and writers per wire format (CVI)
  geode-demo-data    deterministic synthetic risk data and documents
  geode-app          the `geode` binary: wires everything together
docs/
  PHILOSOPHY.md      the charter
  README.md          current guides and historical records
  current/           current subsystem behavior and rationale
  phase-history.md   archived phase and review history
  perf.md            what is measured and what the numbers are
  superpowers/       archived implementation documents
examples/demo-config the config layer `--demo` runs on
scripts/             the mutation harness
assets/              bundled fonts and themes
```

Each crate has its own README with a module map and the rules it pins.

Dependencies are layered and enforced by crate visibility. `geode-shell`
and `geode-data` never depend on each other, a module depends on both but
never on another module, and `geode-app` is the only crate where they all
meet. Only `geode-data` opens a file or a socket.

See `docs/current/architecture.md` for the maintained crate and runtime
ownership model.

Threads are split the same way: the UI thread renders from immutable
snapshots; a query pool owns the DuckDB read connections and delivers
results over channels; one ingest thread owns the writer. The budgets in
the foundation spec §7 are contracts: under 8 ms for a pure-UI action,
under 50 ms for a requery at a million rows, and ingest never drops a
foreground frame.

## Developing

```sh
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
```

CI runs those five on macOS and Windows; keep both building.

A green suite proves less here than you would expect. The query path had
five review rounds each find a silent wrong-data defect no fixture could
reach, so `scripts/mutation-check.sh` breaks one load-bearing behaviour at
a time and runs the suite. A `SURVIVED` line is a branch no test can see.
Run `--changed` after touching the data layer and add an entry for every
behaviour you change; run `--anchors-only` before every merge. Commit
before you mutate.

Start at `docs/README.md` for current guides. `CLAUDE.md` holds workspace
rules and gotchas. For data-path changes, read
`docs/current/data-path.md`; the archived implementation documents are not
required reading.

## Status

Shell, data layer, blotter, diagnostics, config dialogs, named colours,
market-data documents through Part 3, tile stacks, choice lists and the
timeseries data tier are merged. Market-data egress (Part 4) and the
timeseries query and tile (Part 2) are next. Display checks on a real
window are pending for much of the recent work. There are no real vendor
adapters yet: every source shape is built against a simulator first. Current
guides should state their own known limitations as they are migrated.
