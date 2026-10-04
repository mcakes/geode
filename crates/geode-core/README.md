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
| `schema` | Dataset families (`measures`, `document`, `series`, `reference`), columns, roles, grains, and family-specific validation. Readers may retain corrected objects with diagnostics. Grain `Ord` reads coarse < fine. |
| `scope` | Dimension selections, text, expressions, named references, and impossible-state tracking. Composition intersects dimensions, deduplicates names, ANDs expressions, and takes inner text. `Scope::resolve` ANDs named expressions in list order with the existing expression and errors on the first missing or invalid name. Parsing and schema validation are separate; conjunct helpers let the toolbar edit individual terms. |
| `named` | Named expressions from `expressions.toml`. Parse failures remain as invalid entries with reasons so references distinguish broken definitions from missing ones. Unknown columns warn while retaining a parsed expression. |
| `scope::eval` | In-process evaluation of a scope over one row (`RowValues`), for data that never reaches DuckDB. The SQL lowering in `geode-data`'s `scope_sql` is the authority: the `eval_parity` fixture there runs every operator through both and asserts equal row sets, so DuckDB's casts, `ilike` without ESCAPE, NaN ordering and three-valued NULL logic are copied, not reinvented. `Expr::evaluate` is three-valued; `Scope::matches` keeps only TRUE, and refuses a scope that still carries unresolved names. A comparison DuckDB would refuse is an `Err`; `matches` reports the error on the row whose value fails; the SQL fails the whole query, so a caller must refuse the scope if any row errs. `Scope::bind` runs the row-independent refusals and constants once and answers a `BoundScope` that evaluates each row; `Scope::bind_check` answers only the refusals, so a caller over no rows still refuses what the SQL would. |
| `scope::complete` | Forgiving expression tokens, caret context and replacement ranges, schema vocabulary, and warnings for unknown columns or unsupported derived-dimension comparisons. Callers parse separately before accepting expressions. Completion requires a caret on a UTF-8 boundary and does not support column names starting with an ASCII digit. |
| `scopes` | Saved scopes from `scopes.toml`, including named-expression references. Reading and persistence retain names; `Scope::resolve` validates their definitions when used. |
| `groupings` | The nine numbered grouping slots. |
| `dimensions` | Derived dimensions the desk groups by that are not in the source files (`desk` from `book`). |
| `classification` | The editing model for a classification (a derived dimension a person edits), pure. Grid rows over observed and mapped source values; edits that return the whole next object and an `UndoEntry`, whose undo replays over the current object and skips a row changed since; `csv` (an RFC 4180 reader and writer: quotes, BOM, CRLF, blank lines skipped); `import` (header must name `<from>,<name>`, all-blank rows skipped, a source given two labels rejected whole, `MAX_IMPORT_BYTES` and `MAX_IMPORT_ROWS`); `validate` (identifier names, no keyword, `config_version`, column or dimension clash ignoring case; utf8, groupable, never-derived sources; `references` counted for rename and delete). |
| `textfile` | A tile's text file read or write request (`TextFileParams`, keyed by tile and tag) and its `TextFileOutcome`, shared by `geode-data`, which does the I/O, and `geode-shell`, which delivers the answer. |
| `view` | View definitions, presentation, and validation of datasets, joins, column roles, reachability, and grouping. Ungrouped primary dimensions use the coarsest declared grain carrying the column and the entire grouping, shared by validation and compilation. Required failures refuse the view; supported optional failures warn. Derived SQL and sort keys are checked when the generated SQL is bound and executed. |
| `attribution` | Whether a measure can be summed at a grouping level, and how a scope predicate reached it. |
| `grid` | Row and cell-block selections anchored by identity and resolved against current display order. Selection summaries exclude descendants of selected parents to avoid double-counting, and suppress totals for non-additive cells or unsummable columns. |
| `link` | Link-group vocabulary shared by the shell and the modules: `Group` (the four fixed groups, their letter and their session spelling), `Membership` (the group a tile follows and the one it emits into), and `Emission` (an optional scope and `BoardEntry` draft documents). A `BoardEntry` compares its rows by allocation, so an emitter that reuses an `Arc` for an unchanged draft is read as unchanged, and carries a `DraftMark` (`Editing`, `Behind`, `Sent`; `label()` is the follower's word, none for a live edit) that compares too, since the same rows falling behind or being sent is a change. `UNDERLYING` is the one column a group's scope is named by (`underlying_ref`): emitters build their scope with `underlying_scope(underlying)` and readers take the name back with `underlying_of(scope)`, the scope's sole value for that column, so the two sides cannot disagree on the column. Pure: the frame in `geode-shell` holds the state. |
| `context` | `DimensionContext`: the `(column, value)` pairs at a tile's cursor row — every dimension or key with one value there — plus the selected rows' values. NULL, mixed, or not-yet-grouped values are absent, never guessed. `offers` is true when the context holds any one column a kind accepts. |
| `query` | Requests and outcomes for views, distinct values, catalogs, documents, and reference tables; request keys, tags, and as-of parsing. |
| `reference` | Whole-table snapshots for reference datasets. `TableRows` is an adapter's answer, named columns in any order; `conform` checks it against the declaration and returns `ConformedRows` in `document_columns()` order, rows sorted by key. It refuses a repeated column, unequal lengths, no rows, a missing key column, a type mismatch, a non-finite number, a NULL key and a repeated key, so a bad snapshot never replaces the live table. An omitted optional column reads NULL and is named in `missing`; an undeclared column is ignored and named in `extra`. On the read side, `ReferenceData` is a live-only snapshot of read answers (`ReferenceTable`) keyed by dataset, then by the first `key_columns` cells joined with `/`; `lookup(dataset, key, column)` reads `None` for an unknown dataset, key or column and for a NULL cell. `with_table` and `without` return `None` when nothing changed, and equality ignores the generation and source time, so an unchanged republish wakes no observer. A row with a NULL key cell is skipped; a repeated joined key keeps the last row (`conform` refuses repeated keys upstream), and since cells join with `/`, a key cell containing `/` can collide with another multi-column key. |
| `snapshot` | Immutable, `Arc`-shared columnar results, attribution and freshness metadata, and typed cell access. Feature modules can read cells without an Arrow dependency; construction and raw array access also expose Arrow types. |
| `tree` | The parent/child index of a rollup result, built once on the query worker. |
| `sort` | Sibling sort vocabulary shared by the grid tiles: `SortOrder` (asc, desc, abs desc, abs asc), the `s`/`shift+s` key cycles (`cycle`), the header click cycle (`click_cycle`, desc first, a measure's walking the absolute pair too), `on_column` folding an absolute order on a text column to its signed direction, and the `:sort` argument grammar and completions (`parse_args`, `completions`). Each tile holds its own sort and ranks its own rows. |
| `expansion` | The path-keyed open/closed state of a grouped tree (`Path`, `Expansion`), shared by the blotter and the pricer. A path is the grouping values from the root, `None` for NULL, so it survives requery and sibling reordering; `prune_to` drops paths deeper than a new grouping. Named `expansion`, not `tree`, because `tree` is the snapshot's index. |
| `document` | Columnar document rows, keys, attributes, parser/writer traits, and schema validation for feed and application-authored documents. |
| `panel` | Market-data panel vocabulary (`PanelSpec`, `KindActionRegistry`) and the pure `panels` reader: `read_panels` judges each panel alone, `load_panels` also checks it against the schema and document kinds. Every problem refuses the panel with one Error; nothing is guessed. |
| `series` | Timeseries requests, bucket frequencies and rules, aligned results, and fetch provenance. `series::expr` parses arithmetic over source names and resolves references to slot IDs. |
| `pricing` | Instrument, request, `Measure` (14 bumped measures) and `PriceResult` (local and USD), the `Pricer` trait, shifts, market overrides, and local document publications. Implementations live in `geode-pricing`; the pricing library resolves tenors and percent strikes. |
| `vol` | Vol-slice batches and outcomes, coordinates, grids, curve and chain-mapping requests, and the `VolModel` trait. The model supplies every coordinate, volatility, and density; UI modules only prepare and paint the returned values. |
| `clock` | Configured display zone, local-time resolution, and business-day presets. Rejects ambiguous or nonexistent local times; storage timestamps remain UTC. |
| `source_config` | I/O-free source parsing: defaults, dataset-family routing, topic and timestamp-field validation, and field-addressed diagnostics. Source tables replace whole objects across layers. |
| `egress_config` | I/O-free upload-target parsing and field-addressed diagnostics. Invalid targets are skipped; address templates substitute raw key parts joined by `/`. Targets replace whole objects across layers. Runtime workers retain startup configuration until restart. |
| `positions` | Position-system command vocabulary: `MoveLhuParams` (tag, positions, LHU), `CommandOutcome`, the I/O-free `positions.toml` reader (`PositionsSpec`, `from_doc`: one `[service] adapter`; a missing adapter is an error at `positions.service.adapter`, unknown keys warn), and the Move LHU notice wording (`noun`, `sent_notice`, `outcome_notice`) the action and the shell share. Restart-required. |
| `format` | Number formatting (scale, precision, grouping, negative style) shared by the blotter and the market-data panel. |
| `nudge` | Pure numeric-editor stepping at a supplied or inferred precision. Returns text without committing an edit; segmented date fields handle dates. |
| `colour` | Named colors from semantic tokens or OKLCH hue interpolation. Contrast correction targets 3:1 but may fall short for custom themes. Pure; callers supply `Anchors`/`Tokens`. `Definition::from_table` is the one per-entry reader for `colors.toml` and inline value entries. `values`: `value_colors.toml` reader — a text dimension's value → color name, or an inline `{ hue }`/`{ token }` entry keyed `inline {dimension}.{value}`, which `NamedColours::get` resolves and `names()` hides — and `check_value_colors`, which prunes undeclared or non-text dimensions and unknown colors with warnings; `NamedColours::from_config` carries the checked mapping with the definitions. |
| `health` | Source-health states (`Ok`, `Pending`, `PendingTooLong`, `Degraded`, `Failed`). Rollups compare explicit severity ranks and preserve simultaneous reasons; derived `Ord` also sorts reason text. |
| `log` | The in-process log ring every `tracing` layer feeds, `[log]` levels by `geode::*` target, and the runtime level control. |
| `panic` | The thread-local marker that lets the process panic hook tell a contained panic from a fatal one. |

## Features

- `test-support` exposes a `Snapshot` fixture builder accepting plain Rust
  values, a configuration helper that builds a layer without disk I/O, and
  `reference::test_support::reference_dataset`, a reference `DatasetSpec`
  parsed through the schema reader.
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
