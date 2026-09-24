# geode-core

The shared vocabulary of the Geode workspace: the types every other crate
speaks in, without gpui or DuckDB dependencies. Typed readers and merging are
I/O-free; `config::Config::load` and `read_docs` read configuration files.
It sits at the bottom of the dependency graph. Types exchanged between
independent crates live here: the shell and data layer share `Scope`,
`Snapshot`, `Health`, and query values without depending on each other.

Its place in the dependency graph is described in
[`docs/current/architecture.md`](../../docs/current/architecture.md).
Layered document behavior is described in
[`docs/current/configuration.md`](../../docs/current/configuration.md).

## What lives here

| Module | Holds |
|---|---|
| `config` | Builtin → Desk → User TOML loading, recursive merging with whole-object exceptions, and path provenance. File loading collects diagnostics; typed readers validate separately. `load_views` applies dataset and view presentation without rewriting definitions. TOML key order is preserved. |
| `schema` | The declared shape of the desk's data (`datasets.toml`): datasets, families (`measure`, `document`, `series`), columns, roles and measure `grain`. Grain `Ord` reads coarse < fine. |
| `scope` | What every tile is looking at: dimension selections, a text filter and a validated expression, composed with AND. `scope::expr` is the restricted WHERE grammar, parsed against the schema, never raw SQL. |
| `scopes` | Saved scopes (`scopes.toml`). |
| `groupings` | The nine numbered grouping slots. |
| `dimensions` | Derived dimensions the desk groups by that are not in the source files (`desk` from `book`). |
| `view` | View definitions: dataset, joins, columns, derived columns, grouping and sort, as config. |
| `attribution` | Whether a measure can be summed at a grouping level, and how a scope predicate reached it. |
| `query` | Value types shared by both ends of the query path: as-of, query keys and outcomes. |
| `snapshot` | The immutable, `Arc`-shared columnar result the UI reads. Arrow is an implementation detail; nothing outside this file names an Arrow type. |
| `tree` | The parent/child index of a rollup result, built once on the query worker. |
| `document` | The struct-of-arrays rows a parsed market-data document becomes, and the `DocumentKind` trait a parser implements. |
| `source_config` | I/O-free source parsing: defaults, dataset-family routing, topic and timestamp-field validation, and field-addressed diagnostics. Source tables replace whole objects across layers. |
| `format` | Number formatting (scale, precision, grouping, negative style) shared by the blotter and the market-data panel. |
| `colour` | Named colours: a hue on a canonical wheel interpolated in OKLCH between theme anchors, with a 3:1 readability floor. Pure; callers hand in `Anchors`/`Tokens`. |
| `health` | The degradation vocabulary (`Ok`, `Pending`, `PendingTooLong`, `Degraded`, `Failed`). Roll up by `severity_rank`, never by the derived `Ord`. |
| `log` | The in-process log ring every `tracing` layer feeds, `[log]` levels by `geode::*` target, and the runtime level control. |
| `panic` | The thread-local marker that lets the process panic hook tell a contained panic from a fatal one. |

## Features

- `test-support` exposes the `Snapshot` fixture builder (so downstream
  tests never name an Arrow type) and a config-layer test helper. Every workspace crate's dev-dependencies
  turn it on, and this crate dev-depends on itself with it so
  `cargo test -p geode-core` and `cargo test --workspace` build one
  artifact.

## Commands

```sh
cargo test -p geode-core
cargo bench -p geode-core          # config merge and tree-index benches
```

## Rules this crate pins

Source settings and validation outcomes are described in the
[configuration guide](../../docs/current/configuration.md#source-configuration).
Parsing a source does not establish transport availability or runtime support
for its readiness strategy; the data service checks those boundaries.

- Keep typed interpretation and merging free of I/O; configuration file
  loading is a separate entry point. A type that needs to know a gpui or
  DuckDB type does not belong in this crate.
- Every parse failure degrades to a `Diagnostic` and skips the offending
  input. A panic on bad config is a defect.
- Struct-of-arrays throughout (`docs/PHILOSOPHY.md` §6): `Snapshot`,
  `DocumentRows` and the tree index hold columns, never row objects.
