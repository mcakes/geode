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
| `scope` | Dimension selections, text, expressions, named references, and impossible-state tracking. Composition intersects dimensions, deduplicates names, ANDs expressions, and takes inner text. `Scope::resolve` ANDs named expressions in list order with the existing expression and errors on the first missing or invalid name. Parsing and schema validation are separate; conjunct helpers let the toolbar edit individual terms. |
| `named` | Named expressions from `expressions.toml`. Parse failures remain as invalid entries with reasons so references distinguish broken definitions from missing ones. Unknown columns warn while retaining a parsed expression. |
| `scope::complete` | Forgiving expression tokens, caret context and replacement ranges, schema vocabulary, and warnings for unknown columns or unsupported derived-dimension comparisons. Callers parse separately before accepting expressions. Completion requires a caret on a UTF-8 boundary and does not support column names starting with an ASCII digit. |
| `scopes` | Saved scopes from `scopes.toml`, including named-expression references. Reading and persistence retain names; `Scope::resolve` validates their definitions when used. |
| `groupings` | The nine numbered grouping slots. |
| `dimensions` | Derived dimensions the desk groups by that are not in the source files (`desk` from `book`). |
| `view` | View definitions, presentation, and validation of datasets, joins, column roles, reachability, and grouping. Ungrouped primary dimensions use the coarsest declared grain carrying the column and the entire grouping, shared by validation and compilation. Required failures refuse the view; supported optional failures warn. Derived SQL and sort keys are checked when the generated SQL is bound and executed. |
| `attribution` | Whether a measure can be summed at a grouping level, and how a scope predicate reached it. |
| `grid` | Row and cell-block selections anchored by identity and resolved against current display order. Selection summaries exclude descendants of selected parents to avoid double-counting, and suppress totals for non-additive cells or unsummable columns. |
| `launch` | Typed cursor context shared between feature modules. Missing or ambiguous values remain absent; the shell offers targets that accept every populated field. |
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
- `ViewSpec::validate` errors cause the data service to refuse the view by
  name. Joins and columns default to `required = true`. An optional join's
  validation failure warns; optional columns warn for a measure-role mismatch
  or an unreachable dimension. Unknown columns and invalid derived-dimension
  sources still error. A join outside the grouping that supplies no selected
  column warns even when required. Derived SQL and sort keys remain compiler
  concerns; `required = false` does not exempt derived SQL from binding errors.
- `Snapshot` and `DocumentRows` store columns; the tree index stores parallel
  arrays. Snapshot construction prepares concatenated columns and the tree
  before UI delivery.
- An ungrouped dimension's mixed state is a boolean companion column linked by
  `ColumnMeta::mixed_flag`; its value is NULL there. Read it with
  `Snapshot::is_mixed_at` before the value, or a mixed cell reads as blank.
- Cell access preserves NULL separately from zero. Raw value slices omit null
  bitmaps and are suitable only when the caller has established null handling.
  Dictionary codes are local to a column; code-based grouping requires one
  code per value, and cross-snapshot comparisons should use resolved values.
- Request keys identify consumers; submission tags let consumers reject
  superseded answers. Cancellation does not guarantee that no answer arrives.
- Scope expressions are parsed separately from schema validation. A composed
  contradiction remains explicit so an empty intersection cannot become an
  unrestricted query.
