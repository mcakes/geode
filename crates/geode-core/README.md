# geode-core

The shared vocabulary of the Geode workspace: the types every other crate
speaks in, with no I/O, no gpui and no DuckDB. It sits at the bottom of
the dependency graph, so anything two crates that may never depend on
each other need to exchange lives here (the shell and the data layer are
the standing case: `Scope`, `Snapshot`, `Health` and the query value
types all moved down for that reason).

Its place in the dependency graph is described in
[`docs/current/architecture.md`](../../docs/current/architecture.md).
Layered document behavior is described in
[`docs/current/configuration.md`](../../docs/current/configuration.md).

## What lives here

| Module | Holds |
|---|---|
| `config` | Layered TOML configuration: Builtin → Desk → User, deep-merged with per-path provenance. Invalid config never panics; failures are `Diagnostic` values and the bad input is skipped. `toml`'s `preserve_order` is on, so a doc's key order is file order everywhere it is iterated. |
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
| `source_config` | `sources.toml`: one named table per source. |
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

- Nothing here opens a file, a socket or a window. A type that needs to
  know a gpui or DuckDB type does not belong in this crate.
- Every parse failure degrades to a `Diagnostic` and skips the offending
  input. A panic on bad config is a defect.
- Struct-of-arrays throughout (`docs/PHILOSOPHY.md` §6): `Snapshot`,
  `DocumentRows` and the tree index hold columns, never row objects.
