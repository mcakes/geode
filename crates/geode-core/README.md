# geode-core

The shared vocabulary of the Geode workspace: the types every other crate
speaks in, without gpui or DuckDB dependencies. Configuration merging and
schema interpretation work from memory. `config::Config::load` and `read_docs`
read configuration files; `Clock::machine` looks up and caches the system zone.
It sits at the bottom of the dependency graph. Types exchanged between
independent crates live here: the shell and data layer share `Scope`,
`Snapshot`, `Health`, and query values without depending on each other.

Its place in the dependency graph is described in
[`docs/current/architecture.md`](../../docs/current/architecture.md).
Layered document behavior is described in
[`docs/current/configuration.md`](../../docs/current/configuration.md).
Reader defaults, partial validation, and presentation rules are described in
[typed configuration documents](../../docs/current/typed-documents.md).

## What lives here

| Module | Holds |
|---|---|
| `config` | Builtin → Desk → User TOML loading, recursive merging with whole-object exceptions, and path provenance. File loading collects diagnostics; typed readers validate separately. `load_views` applies dataset and view presentation without rewriting definitions. TOML key order is preserved. |
| `schema` | Dataset families (`measures`, `document`, `series`), columns, roles, grains, and family-specific validation. Readers may retain corrected objects with diagnostics. Grain `Ord` reads coarse < fine. |
| `scope` | Dimension selections, text, expressions, and impossible-state tracking. Components combine with AND within a query; across layers, dimensions intersect, expressions AND, and inner text replaces outer text. Expression parsing and schema validation are separate. `Expr::conjuncts` splits an expression into its top-level `and` terms and `Expr::from_conjuncts` rebuilds a left-folded chain from them, which is how the toolbar edits one term at a time. |
| `scopes` | Saved scopes (`scopes.toml`). |
| `groupings` | The nine numbered grouping slots. |
| `dimensions` | Derived dimensions the desk groups by that are not in the source files (`desk` from `book`). |
| `view` | View definitions: dataset, joins, columns, derived columns, grouping and sort, as config. `ViewSpec::validate` resolves every reference the compiler will resolve: the primary and join datasets, each join's keys against the joined dataset's grains and against the grouping, selected columns, a measure column's role, a dimension column's reachability, and grouping columns. Derived SQL and sort keys remain the compiler's. |
| `attribution` | Whether a measure can be summed at a grouping level, and how a scope predicate reached it. |
| `query` | Requests and outcomes for views, distinct values, catalogs, and documents; request keys, tags, and as-of parsing. |
| `snapshot` | Immutable, `Arc`-shared columnar results, attribution and freshness metadata, and typed cell access. Feature modules can read cells without an Arrow dependency; construction and raw array access also expose Arrow types. |
| `tree` | The parent/child index of a rollup result, built once on the query worker. |
| `document` | Columnar document rows, keys, attributes, parser/writer traits, and schema validation for feed and application-authored documents. |
| `series` | Timeseries requests, bucket frequencies and rules, aligned results, and fetch provenance. `series::expr` parses arithmetic over source names and resolves references to slot IDs. |
| `pricing` | Option definitions, shifts, market overrides, pricing requests and outcomes, the `Pricer` interface, and local document publications. Implementations live in `geode-pricing`; the pricing library resolves tenors and percent strikes. |
| `clock` | Configured display zone, local-time resolution, and business-day presets. Rejects ambiguous or nonexistent local times; storage timestamps remain UTC. |
| `source_config` | I/O-free source parsing: defaults, dataset-family routing, topic and timestamp-field validation, and field-addressed diagnostics. Source tables replace whole objects across layers. |
| `egress_config` | I/O-free upload-target parsing and field-addressed diagnostics. Invalid targets are skipped; address templates substitute raw key parts joined by `/`. Targets replace whole objects across layers. Runtime workers retain startup configuration until restart. |
| `format` | Number formatting (scale, precision, grouping, negative style) shared by the blotter and the market-data panel. |
| `nudge` | Pure numeric-editor stepping at a supplied or inferred precision. Returns text without committing an edit; segmented date fields handle dates. |
| `colour` | Named colors from semantic tokens or OKLCH hue interpolation. Contrast correction targets 3:1 but may fall short for custom themes. Pure; callers supply `Anchors`/`Tokens`. |
| `health` | Source-health states (`Ok`, `Pending`, `PendingTooLong`, `Degraded`, `Failed`). Rollups compare explicit severity ranks and preserve simultaneous reasons; derived `Ord` also sorts reason text. |
| `log` | The in-process log ring every `tracing` layer feeds, `[log]` levels by `geode::*` target, and the runtime level control. |
| `panic` | The thread-local marker that lets the process panic hook tell a contained panic from a fatal one. |

## Features

- `test-support` exposes a `Snapshot` fixture builder accepting plain Rust
  values and a configuration helper that builds a layer without disk I/O.
  Downstream tests enable this feature as needed. This crate's self
  dev-dependency enables it for its own tests and benchmarks too.

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

- Keep configuration merging and schema interpretation separate from file
  loading. Clock configuration can use the cached system-zone lookup when no
  zone is supplied. Shared types must not require gpui or DuckDB dependencies.
- Configuration readers return diagnostics with usable values. Depending on
  the rule, invalid input is skipped, defaulted, or retained with a warning;
  a diagnostic does not imply that the entire document was rejected.
- `ViewSpec::validate` is the exception, and it is a gate rather than advice: an
  error there means the view cannot be honoured, and the data service refuses it
  by name instead of querying it. A warning means the author declared the failing
  join or column `required = false` and it was dropped, or that a join supplies
  no column and is dead weight; the view still serves. A column's `kind` defaults
  to `measure` and `required` defaults to true, so a declaration that says
  nothing says "I meant this". What validate still leaves to the compiler is
  derived SQL, which DuckDB's binder rejects, and sort keys.
- `Snapshot` and `DocumentRows` store columns; the tree index stores parallel
  arrays. Snapshot construction prepares concatenated columns and the tree
  before UI delivery.
- Cell access preserves NULL separately from zero. Raw value slices omit null
  bitmaps and are suitable only when the caller has established null handling.
  Dictionary codes are local to a column; code-based grouping requires one
  code per value, and cross-snapshot comparisons should use resolved values.
- Request keys identify consumers; submission tags let consumers reject
  superseded answers. Cancellation does not guarantee that no answer arrives.
- Scope expressions are parsed separately from schema validation. A composed
  contradiction remains explicit so an empty intersection cannot become an
  unrestricted query.
