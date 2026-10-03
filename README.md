# Geode

Geode is the everything-tool for an index exotic equity derivatives desk:
risk, pricing, execution and data visualisation in one permanent,
keyboard-driven shell. It is written in Rust on gpui (Zed's UI framework)
and gpui-component, with DuckDB as the store.

New to Geode? Start with the [user guide](docs/user-guide.md), a guided demo
walkthrough of tiles, shared scope, grouping, and the everyday workflow.

Three principles from the charter (`docs/PHILOSOPHY.md`) shape everything
in this repository:

- **A lens, not a brain.** The shell and UI modules only shape views:
  grouping, filtering, aggregating and joining supplied data. Financial
  computation stays behind request/outcome contracts, including calculation
  crates linked into the binary.
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

The toolchain is stable Rust (`rust-toolchain.toml`). Building also requires
a native C/C++ toolchain: DuckDB is compiled from bundled source. On macOS,
install the Xcode Command Line Tools; the full Xcode application is not
needed because Metal shaders compile at runtime. CI covers macOS and Windows.

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
| User | `$APPDATA/geode` when set, otherwise `$HOME/.config/geode` |

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
| `mod+shift+h/j/k/l`, `mod+s` | pull a neighbour into the tile's stack, split a stack into tiles |
| `ctrl+1..9`, `ctrl+0` | grouping slots, clear |
| `mod+g`, `mod+p`, `mod+t` | grouping picker, dimension picker, as-of |
| `mod+/`, `mod+z`, `mod+shift+z` | scope text, scope undo, redo |
| `mod+u` | choose the focused tile's link group |
| `mod+d` | open or close the Diagnostics page |
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
  geode-tile         shared tile headers, interaction, caches and query coordination
  geode-widgets      shared application controls below shell and feature crates
  geode-chart        chart geometry, preparation and painting
  geode-blotter      any view as a collapsible keyboard-driven hierarchy
  geode-marketdata   market-data document panels with an edit draft (CVI)
  geode-timeseries   fetchable series, expressions, statistics and chart tile
  geode-volslice     vol smiles per expiry: CVI, a group's draft and the chain
  geode-diagnostics  the diagnostics page over health, generations, config and the log
  geode-guide        offline user-guide tile with section navigation and find
  geode-documents    typed parsers and writers per document wire format
  geode-pricing      implementations of the pricing trait
  geode-pricer       the line pricer: pure sheet core and its tile
  geode-nemo         row-menu links to the external Nemo app
  geode-positions    row-menu commands for a configured position service
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
meet. Source access and DuckDB connections belong to `geode-data`; config,
session, log, crash-report, and demo-fixture I/O have their own owners.

See `docs/current/architecture.md` for the maintained crate and runtime
ownership model.

Threads are split the same way: the UI thread renders from immutable
snapshots; a query pool owns the DuckDB read connections and delivers
results over channels; one ingest thread owns the writer. The budgets in
the current architecture guide are contracts: under 8 ms for a pure-UI
action, under 50 ms for a requery at a million rows, and ingest never drops
a foreground frame.

## Developing

```sh
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo bench --workspace --no-run
cargo check -p geode-shell --features test-support --all-targets
```

CI runs those five on macOS and Windows. The macOS job first runs the
mutation checker's Python unit tests and
`zsh scripts/mutation-check.sh --anchors-only` to validate mutation anchors
and test-name filters.

A green suite can miss wrong-data behavior when its fixture cannot reach the
relevant branch. `scripts/mutation-check.sh` breaks one load-bearing behavior
at a time and runs the named test. A `SURVIVED` line means that named test
did not detect the injected change. Run targeted entries or `--changed`
after changing a correctness contract, and add entries for new contracts;
run `--anchors-only` before every merge. Commit or otherwise preserve your
work before running mutations: the harness edits tracked files in place.

Start at `docs/README.md` for current guides. `CLAUDE.md` holds workspace
rules and gotchas. For data-path changes, read
`docs/current/data-path.md`; the archived implementation documents are not
required reading.

## Status

The shell, data service, blotter, diagnostics, configuration dialogs,
market-data editor, timeseries viewer, chart, shared date-time field, and
line-pricer tile are built, including market-data uploads to configured egress
targets and automatically saved pricer sheets. There are no production vendor
adapters; demo sources exercise each supported source
shape. Some recent UI paths still need real-window display checks. The current
subsystem guides record their specific limitations.
