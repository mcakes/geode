# Configuration

Geode treats configuration as product data. Views, schemas, sources, keymaps,
themes, saved scopes, and presentation choices are readable TOML documents
that can be shared at desk level and overridden by one user.

## Layers and provenance

Configuration is loaded in three layers, from lowest to highest precedence:

1. **Builtin** defaults compiled into the application.
2. **Desk** configuration from `$GEODE_DESK_CONFIG`.
3. **User** configuration from `$APPDATA/geode` when `APPDATA` is set,
   otherwise `$HOME/.config/geode`. This is environment precedence, not a
   platform check; without either variable no user directory is resolved.

The background collector reads the same desk and user layers over the
builtin data layer, at each acquire; see [background
collector](#background-collector).

Tables merge recursively. Scalars, arrays, and values of a different type
replace the lower-layer value at the same path; unrelated values remain.
Provenance records the winning layer at a leaf or whole-object root, and
`Config::explain` checks the requested path and then its recorded ancestors.
TOML order is preserved throughout the workspace because column and row order
are part of several document contracts.

Top-level entries in `views`, `view_presentation`, `dataset_presentation`,
`layouts`, `groupings`, `scopes`, `datasets`, `sources`, `egress`, `dimensions`,
`colors`, `expressions`, `pricer_templates`, `panels`, `scenarios`, `watchlists`, and
`overrides` replace whole named objects.
Overriding one source therefore requires its complete configuration, including
required fields; omitted fields do not inherit from the lower-layer source.

In `vol_views` alone, a higher layer also sets the order of the top-level
tables it defines: the positions those tables already hold are refilled in
that layer's order, tables the layer does not define stay where they are, and
new tables follow at the end. Only tables trade slots; the top-level values
(`config_version`, `default`) are not objects and take part in no order, so a
`default` a user file adds is appended rather than taking a tab's slot and
pushing that tab to the end. The tabs are picked by digit in document order,
and the Vol views dialog reorders them by writing the user layer in the new
order, which needs the merged order to follow it. Every other document keeps
each key where the lowest defining layer put it.

`value_colors` replaces whole at depth 2. A dimension's values merge per value,
so a user entry for one value keeps the lower layers' other values. One value's
entry (a name, `"none"`, or an inline table) is replaced whole, so a user
`{ hue = 30 }` over a desk `{ token = "warning" }` is that hue and never a
table with both keys. An entry of `"none"` clears a lower layer's color.

```toml
[underlying_ref]
SPX  = "blue"                       # a named color from colors.toml
NDX  = { hue = 210 }                # inline, theme-relative
RUT  = { hue = 30, tone = "light" }
DAX  = { token = "warning" }        # inline semantic token
FTSE = "none"                       # clears a lower layer
```

An inline table follows the `colors.toml` definition rules exactly: `hue`
0–360 (360 reads as 0) with an optional `tone` (`normal` | `light`), or
`token`, never both and never neither. `tint_sign` is refused because a value
has no sign. Every refusal is an error at the entry's path
(`value_colors.{dimension}.{value}`, plus `.hue` or `.token` for a field
error) and drops the entry; an invalid `tone` warns and falls back to normal.

Disk loading reads immediate `*.toml` children in sorted path order. Missing
or unreadable directories and failed directory entries are silently skipped.
An individual file read or TOML parse failure produces an error diagnostic
and skips that file. `config_version` must be the integer `1`; an unsupported
value skips the file with an error, while an absent stamp warns and assumes
version 1. Compiled builtin documents bypass the version check.

Renamed documents keep loading under their old file name. A layer directory
holding only `colours.toml` loads it as the `colors` document with a warning
asking for the rename. When one directory holds both `colors.toml` and
`colours.toml`, the old file is ignored with a warning naming it; the current
file is never merged with the stale copy. "Holds" means the file exists: a
`colors.toml` that fails to read or parse keeps its own error and still
shadows the old file, which never stands in for it. `config::RENAMED_DOCS` lists these
pairs. Runtime writes always target the current name: the first
`config_write` edit of `colors` in a user directory holding only
`colours.toml` starts from the old file's content, writes `colors.toml`, and
then removes the old file. If that removal fails, the next load ignores the
leftover copy with the both-files warning.

`Config::read_docs` reads the layers; `Config::from_docs` merges supplied
documents without I/O or typed validation. The latter preserves input order
within each document name, so its caller must supply precedence order and
retain any read diagnostics. `Config::load` combines both steps.

## Documents

The main configuration documents have distinct owners:

| Document | Defines |
|---|---|
| `app.toml` | Theme, primary modifier, UI settings, logging, time zone, link groups, pricing, vol model, and timeseries settings |
| `datasets.toml` | Dataset families, columns, roles, types, grains, retention, local publication, `computed`, and a measures dataset's `axes` (each axis column's `axis` kind and `order`) |
| `views.toml` | Queryable views, joins, columns, expressions, grouping, and sorting |
| `sources.toml` | File, subscription, fetch, reference snapshot, and history sources with readiness and adapter settings |
| `egress.toml` | Upload targets: adapter and a per-document address template |
| `positions.toml` | The one position service: the adapter that takes position commands such as Move LHU |
| `panels.toml` | Market-data panels: the dataset, document kind, layout, formats, and kind actions of each panel tile kind |
| `scenarios.toml` | Scenario tile shapes: dataset, axes down and across, the painted measure, diff and starting paint of each scenario tile kind (restart-required) |
| `pricer_templates.toml` | Named option-package templates used by the pricer's shorthand |
| `dimensions.toml` | Derived dimensions used for grouping and scope; each entry is what the interface calls a classification |
| `groupings.toml` | The nine shared grouping slots |
| `scopes.toml` | Named scopes |
| `expressions.toml` | Named scope expressions, referenced by name from a saved scope or the frame |
| `watchlists.toml` | Named lists of underlyings: manual names, exclusions, and rules deriving names from a dataset under a saved scope or expression |
| `vol_views.toml` | The VolMonitor's tabs: named column lists over the board's measures (hot) |
| `colors.toml` | Named semantic data colors |
| `value_colors.toml` | A color per value of a text dimension: a colors.toml name or an inline { hue } / { token } table |
| `dataset_presentation.toml` | Desk-level column presentation between schema and view overrides |
| `view_presentation.toml` | Per-view column order, visibility, widths, and formatting overrides |
| `keymap.toml` | User bindings layered over builtin and module bindings |
| `overrides.toml` | Intentional drift accepted from the configuration dialogs |
| `session.toml` | Layout and transient session state; excluded from reload change detection |

Session state has its own reader. The generic config loader still reads any
`session.toml` in a layer directory; changing it alone does not trigger reload,
but it can be included when another watched file changes. See
[session persistence](shell.md#session-format) for its format, restoration,
and save guarantees.

`watchlists.toml` reloads without restart: an edit re-folds every list and
resolves again the ones whose definition changed. Rules are validated
against the schema the data service started with, not an edited `datasets`
document awaiting restart, so a rule over a dataset added since launch is a
rule error naming the restart until then. See
[watchlists](typed-documents.md#watchlists) for the grammar and the reader's
rules.

The schema declares meaning rather than storage details. Measure columns name
their aggregation grain. Document columns distinguish keys, attributes, and
typed values. Series datasets describe identities and time/value columns.
Reference datasets (`family = "reference"`) declare a utf8 `key` and plain
attributes for a whole table a snapshot source replaces on each poll; their
rules are in [dataset families](typed-documents.md#dataset-families-and-validation).
History datasets (`family = "history"`) declare a `key` of utf8 or f64
dimensions and f64 values for timestamped observations a history source
answers per key on demand; the rules are in the same section. A view
cannot name a history dataset: it is read by key through a history request.
Views refer to those declarations and may narrow presentation without changing
the underlying dataset.

A view column's `kind` is `measure` (the default), `dimension`, or `derived`.
A join or column also accepts a boolean `required`, defaulting to true;
non-boolean values leave the default in force. Required declarations that fail
validation refuse the view when queried, with a diagnostic naming the view and
column or join.

Use `kind = "dimension"` for a dimension instead of relying on the measure
default. `required = false` permits dropping an unusable join, a column whose
measure role does not match, or a dimension column unreachable under the
grouping. It does not suppress every validation error: unknown columns and
invalid derived-dimension sources still refuse the view. Derived SQL is
checked during compilation and is unaffected by this flag. See
[view validation](typed-documents.md#views-and-presentation) and
[query refusals](data-path.md#queries-and-time-travel) for the boundaries.

An ungrouped `dimension` column can still be displayed when a declared grain
of the primary dataset carries it alongside the whole grouping. Each tree row
shows its value when all contributing rows agree, `mixed` when they disagree,
and blank when all values are NULL. If no grouping, join, or carrying grain
can supply it, a required column refuses the view. See
[ungrouped dimension columns](data-path.md#ungrouped-dimension-columns).

## Validation boundaries

Typed readers in `geode-core` interpret supplied documents without I/O and
return values plus diagnostics. Its `Config::load` and `Config::read_docs`
entry points do read files. Consumers derive runtime state from the loaded
documents; modules do not reopen configuration files. File loading does not
run every typed reader, and the reload rejection boundary is described below.

The [typed-document reference](typed-documents.md) describes each reader's
accepted values, defaults, validation scope, and partial-result behavior.

`load_views` resolves each presentation property in this order: kind default,
view definition, dataset presentation, then view presentation. It checks named
color references at each definition site before overlays can hide an invalid
value or reorder columns. Definition diagnostics use file column indices;
overlay diagnostics use column names. The raw merged `views` document remains
unchanged so dialogs can persist definitions and presentation separately. This loader
returns view and overlay diagnostics, including unknown-color warnings;
schema and color-definition diagnostics are reported by other callers.

Scope expressions use a restricted grammar validated against the schema. They
are never raw SQL. A saved scope is checked by `Scope::validate` wherever it
is loaded; both the frame's expression dialog and the Scope dialog's
definition step additionally check a typed draft at Enter, so an unknown
column or a disallowed operator on a derived dimension is refused with the
field still open rather than accepted and left to fail later at query time
(see [input-and-dialogs.md's Frame expression](input-and-dialogs.md#frame-expression)
and [Definition step](input-and-dialogs.md#definition-step)).
Source adapter names, document kinds, module keymap fragments, pricer
names, and market-data panels depend on what the assembled application has
registered. The app and data service check provider capabilities and panel
composition at startup. Source, document-kind, pricing-adapter, vol-model,
and panel changes require restart; a reload does not rebuild those runtime
registries or repeat their startup capability checks. Keymap compilation
does run on reload, against the existing action registry.

`expressions.toml` holds named scope expressions: an expression text saved
under a name so a saved scope or the frame can refer to it instead of copying
it (`geode_core::named::NamedExpressions`). Each entry is parsed at load time.
One that fails to parse is kept as `Invalid`, carrying its text and parse
reason, rather than dropped, so a reference to it can report "invalid" rather
than "missing" — a deleted name and a broken definition must not look alike.
An expression that parses but names a column the current schema lacks stays
`Valid` and only warns; the column error surfaces the same way any scope
expression's does, at query time. A scope's own list of names is read and
persisted without validating them against this document at all: resolution,
not the reader, is what a missing or invalid name fails against (see
[the shared frame](shell.md#the-shared-frame)).

Schema changes do not migrate an existing DuckDB database. At open, each
payload table is compared with the DDL its dataset declares (names, types,
number and order of columns; DuckDB type aliases and identifier case are not
differences). A table that differs is drift: the dataset is refused for the
run — its sources report `Failed` with `schema drift in '<dataset>': <diff>;
delete the table or fix the dataset`, loads into it and reads of it are
refused, and one error diagnostic names the tables. Recover by deleting the
drifted table (or the demo database) or by correcting `datasets.toml`, then
restarting; `datasets` is restart-required, so drift is decided once per
run. A table that does not exist yet is created and is never drift.

Every build declares two datasets in its builtin layer: `pricer_sheets`, the
line pricer's local document dataset, and `pricer`, its computed vocabulary
dataset (see [the line pricer](features.md)). `datasets` merges per dataset
name, so a demo, desk, or user `datasets` document adds its datasets beside
them, and a `datasets` document always exists. The app keeps its own
declaration of both — `pricer_sheets` because its tables are written
positionally, `pricer` because a differing declaration would change what a
view, scope, or grouping over the pricer means: a layer that redeclares
either identically is accepted silently, and any other redeclaration —
different columns, order, or flags, or one invalid enough that the reader
dropped it — is replaced by the builtin declaration at startup and on reload,
with an error diagnostic naming the redeclaring layer and file. The Schema
dialog still lists both for presentation edits; a redeclaration can only
come from a `datasets` document. Local
datasets are not offered as the dataset choice in the Views and Sources
dialogs: no view reads the app's own documents, and the source reader refuses
a local dataset. A value that already names one is kept, as any current value
is.

A measures-family dataset may set `computed = true`: a module answers for it
in process. No source may feed it (the source reader refuses one), no table is
created, and any query naming it is refused with `dataset '<name>' is computed
by a module and has no tables`. Its value is vocabulary: its columns take part
in scope completion, groupings and the Views dialog's dataset choice exactly as
a stored dataset's do, while the Sources dialog and the frame's value pickers
leave it out. `computed` on a document or series dataset is an error and is
cleared. The line pricer's `pricer` dataset is the one computed dataset every
build declares.

A measures-family dataset may set `axes`, naming columns of `role = "axis"`
that extend the key of its measure grains (see
[axes on measures datasets](typed-documents.md#dataset-families-and-validation)).
The reader drops the whole dataset, with an error at the offending path, for
an axis column without `axis = "bucket"` or `"scenario"`, an unknown kind or
`order`, an axis that is not utf8 or f64, `order` on an f64 axis, an `axes`
entry naming an undeclared or non-axis column, an axis column not listed in
`axes`, an axis named like a grain key column, and `axes` on a dataset that
declares no measure. Dropping the dataset rather than the column is
deliberate: a table built without a declared axis would merge its buckets
under one key, and every sum over it would be plausible and wrong.

Several reports can feed one measures dataset, each from its own directory
source (the demo's five term reports, one per measure, all feed
`term_sensitivities`). Two rules make that work. Each source lists the measures its files carry
in [`measures`](#source-configuration). A measure the file lacks and the
list omits is another report's: it reads as NULL with no degradation and
no note. A listed measure the file lacks keeps the schema's `required`
rule, so a truncated report still degrades. Each source's `batch_pattern`
must yield a batch no other report's file yields for the same book,
because a partition is (dataset, batch, book) and a load replaces its
partitions' live rows: two reports under one batch delete each other's
rows on every load. A query sums each measure over every partition, and
SQL `sum` skips the other reports' NULLs, so no report zeroes or blanks
another's totals.

Changing a report's batch (splitting one report into several, or editing
a `batch_pattern`) leaves the partitions under the old batch live. A load
replaces only its own (dataset, batch, book) partitions, and nothing
retires a batch no source produces any more: retention sweeps archive
tables only, never live ones, so the old partitions never age out. Their
measures add into the new partitions' with `Ok` health and no note, a
plausible wrong total. After such a change, delete the store (for the
demo, `$TMPDIR/geode-demo/<rows>-42/`) so every partition is rebuilt from
the current sources.

The `pricer` declaration marks its nine utf8 dimensions `textual = true`:
`sheet`, `template`, `underlying_ref`, `expiry`, `option_type`, `currency`,
`barrier_type`, `priced_at` and `status`. Its keys (`position_ref`,
`instrument_ref`) are not textual. The frame's text filter searches exactly
these columns on a pricer sheet, as it searches a stored dataset's textual
columns (see [the evaluator's parity contract](data-path.md#queries-and-time-travel)).
Derived dimensions in `dimensions.toml` apply to the pricer's scope as to a
blotter's: a `dimensions` edit alone reloads the pricer, and the reload hands
its factory the new dimensions before every open tile re-applies its scope.

The pricer groups by the `pricer` dataset's groupable columns — the columns
its grains carry, which are its keys (`position_ref`, `instrument_ref`) and
its dimensions (`underlying_ref`, `expiry`, `strike` and the rest; `:group`
completes the list), never a measure — and by derived dimensions over those
dimensions. A derived
dimension over either key is not groupable in the pricer: the keys hold the
sheet's own `p<id>` / `i<id>`, which a desk map can never name. A grouping
level outside this vocabulary is dropped by the tile and struck through in
its header (see [grouping](features.md#grouping)).

## Source configuration

`sources.toml` has one top-level table per source, such as `[risk_files]`.
Every source needs a declared, non-local dataset. The `adapter` setting and
dataset family select its runtime path:

| Shape | Selection and required settings |
|---|---|
| Directory | `adapter = "csv_dir"` (the default), with `paths` globs. A series dataset is rejected. |
| Subscription | Another adapter over a document dataset, with a `document` kind and nonempty `topics`. |
| Fetch | Another adapter over a series dataset. No document kind or topic list is needed. |
| Snapshot | Another adapter over a reference dataset, with a nonempty `table` the adapter reads whole on each poll. A directory source over a reference dataset is rejected. |
| History | Another adapter over a history dataset. `table` names the adapter-side table and defaults to the dataset name. A directory source over a history dataset is rejected. |

The shared reader returns usable sources plus diagnostics addressed to
`sources.<name>.<field>`. Missing/unknown datasets, local datasets, incompatible
adapter/dataset shapes, and invalid required subscription settings skip the
source with an error. A directory source with no usable paths is skipped with
an idle warning, allowing an incomplete definition to be saved. Runtime
adapter capability and document-kind compatibility are checked when the
service opens the source; an unservable source does not stop the others.

Directory defaults are sentinel readiness, a 30-second poll interval, a
10-minute pending timeout, and `latest_risk` priority. Other priorities are
`latest_other` and `backfill`. `batch_pattern` is a regex with a named `batch`
capture applied to the CSV filename stem; it should exclude the date when
successive business dates belong to the same partition. An invalid pattern
warns and is ignored; an absent or nonmatching pattern uses the whole stem.
`readiness = { stable_mtime = 3 }` parses, but discovery does not implement
that strategy and reports matched candidates as unusable.

`measures` (directory sources only) lists the measures the source's files
carry, for a measures dataset fed by several reports. Absent, the source
carries every measure. Each entry must name a declared measure of the
dataset, and the list must be nonempty; otherwise the source is refused
with an Error at `sources.<name>.measures`, because a misspelt name would
read the real measure as another report's and a file missing it would
load as healthy. On an adapter-backed source it warns and is ignored. The
Sources dialog does not edit it and keeps it on save.

Subscription defaults are `coalesce = "500ms"` and `source_time = "receive"`.
The coalescing interval spaces releases to ingest for each document key;
`"0"` disables coalescing, without guaranteeing message delivery or immediate
publication. `source_time = "document:<field>"` requires a document-level
`date` or `utf8` attribute. At receipt, a date becomes midnight UTC and a
string must parse as RFC 3339 with its offset. Schema validation cannot ensure
that a particular message supplies a valid value.

Two subscription settings govern [recovery on
subscribe](data-path.md#recovery-on-subscribe), which asks the transport for
the latest document on each recorded topic at start and after a reconnect.
`recover_timeout` (default `"10s"`) is how long the receiver waits for
replies; one second of grace is added, so `"0"` leaves a one-second window
and requests still go out. `recover_max_age` (default `"7d"`) prunes, at
open, recorded topics last received longer ago than this; they are not
asked for. `"0"` prunes every recorded topic, so nothing is recovered at
start. Both are subscription-only settings, warned and ignored on other
shapes.

The snapshot default is `poll_interval = "5m"`. A zero interval warns and uses
`5m`, because it would reread the whole table in a tight loop. A missing or
empty `table` rejects the source. `table` warns and is ignored on every other
shape. A snapshot source has no priority: the ingest runner takes snapshots
ahead of every file, so `priority` warns as ignored by a snapshot source. A
reference dataset has at most one snapshot source: a snapshot publishes the
whole table as one partition, so two sources would replace each other's rows
on every poll. The second source in file order over a dataset already filled
is rejected at `sources.<name>.dataset`, naming the first.

A history source is asked per key on demand, so it has no poll: a
`poll_interval` on one warns and is ignored, as every other setting that
does not apply to its shape does. A `table` that is empty or not a string
warns and falls back to the dataset name. `priority` is accepted without a
warning but orders nothing: the runner takes history ahead of every file. A
history dataset has at most one source, because coverage is kept per key and
not per source: a second source would answer asks the first already covered,
and a read could not say whose rows it returned. The second in file order is
rejected at `sources.<name>.dataset`, naming the first.

Topic patterns use `/`-separated levels. A whole `*` matches one level; a
final `>` matches one or more trailing levels. Empty patterns, empty pattern
levels, and a non-final `>` level reject the source. Other text is literal.
Directory-only settings warn and are ignored on adapter-backed sources, except
`poll_interval` on a snapshot source; subscription-only settings warn and are
ignored on directory, fetch, snapshot and history sources. Parsed directory paths are
cleared on adapter-backed sources.

Duration strings accept nonnegative integers with `ms`, `s`, `m`, `h`, `d`,
or `y`, plus bare `"0"`; a day is 24 hours and a year is 365 days. Invalid
duration values warn and use the setting's default. Unrecognized readiness,
priority, and source-time strings also warn and fall back to their defaults;
a recognized `document:<field>` with an invalid field instead rejects the
source. The duration warning currently lists only `s`, `m`, `h`, and `ms`,
although the parser also accepts `d` and `y`.

Source and dataset edits require restart to rebuild runtime workers. See
[`source_config.rs`](../../crates/geode-core/src/source_config.rs) for parsing
and [source discovery and adapters](data-path.md#source-discovery-and-adapters)
for runtime readiness, delivery, and failure behavior.

### Credentials

Shipped adapters require no credentials. Adapters that need them must read
secrets from the environment. Layered configuration may name the environment
variable, but must not contain the secret: these files are shared, diffable,
and writable by the application.

## Egress configuration

`egress.toml` has one top-level table per upload target, such as
`[sophis]`. Each target names an `adapter` and a `documents` table mapping a
document dataset name to an address template:

```toml
[sophis]
adapter = "demo_bus"
[sophis.documents]
cvi_params = "marketdata/cvi/{key}/NOTIFY"
dividend_schedule = "marketdata/dividend/{key}/NOTIFY"
```

Targets replace whole named objects across layers. An override must repeat
its adapter and documents; omitted fields do not inherit from a lower layer.
`{key}` is replaced literally by document-key parts joined with `/`, without
escaping. A template without `{key}` is a fixed address for every key of that
document. Empty address strings and unknown placeholder text are not rejected
by this reader; transport-specific address validation belongs to the adapter.

The typed reader skips non-table targets with a warning. A missing or
non-string adapter, missing/non-table/empty documents table, non-document
dataset name, or non-string address produces an error and drops that whole
target. Diagnostics use `egress.<target>` and a known field or document name.
Other targets remain usable. The reserved `config_version` entry is skipped.

Startup adapter resolution separately drops targets whose adapter is unknown
or exposes no egress capability. The app passes the surviving target names and
accepted documents to panel factories. This does not guarantee that a worker
starts successfully or a transport accepts an upload. See
[egress and uploads](data-path.md#egress-and-uploads) and
[`egress_config.rs`](../../crates/geode-core/src/egress_config.rs).

Egress changes require restart. Reload can update the active configuration
representation, but running workers and panel target lists retain their startup
configuration. The restart indicator compares the layered egress document with
its startup baseline; restoring those inputs clears the indicator.

## Position service configuration

`positions.toml` names the one position service, the adapter that takes
position commands (the blotter row menu's [Move
LHU](features.md#move-lhu)):

```toml
[service]
adapter = "demo_positions"
```

The document is not in the whole-object list above, so layers merge it
recursively: a higher layer's `adapter` replaces a lower one's.

No document, or a document without a `[service]` table, configures no
service and reports nothing beyond its unknown keys, which still warn as
below without a `[service]`. A `service` entry that is not a table is an
error at `positions.service`; a `[service]` without a string `adapter` is an
error at `positions.service.adapter`. Either configures no service. Unknown
keys, at the top level or inside `[service]`, are warnings at
`positions.<key>` or `positions.service.<key>` and are ignored. The reserved
`config_version` entry is skipped.

Startup adapter resolution separately drops a service whose adapter is not
registered in this build or exposes no position capability, with an error at
`positions.service.adapter`. Without a surviving service, Move LHU is
disabled with `no position service configured`. Resolution does not
guarantee that the worker starts or that the position system accepts a
command. See [position commands](data-path.md#position-commands) and
[`positions.rs`](../../crates/geode-core/src/positions.rs).

Position-service changes require restart. The worker and the action's
enabled state keep their startup configuration. The restart indicator
compares the layered positions document with its startup baseline; restoring
those inputs clears the indicator.

The `--demo` layer supplies `adapter = "demo_positions"`, the demo position
system that rewrites the demo risk CSVs.

## Market-data panels

`panels.toml` defines the market-data panel tile kinds. Each top-level table
is one panel, and its name is the tile kind: `[cvi]` is the `cvi` kind, its
add-tile action is `tile::add_cvi`, and a saved session records its tiles as
`module = "cvi"`. The builtin layer ships `cvi` and `dividend`
([`builtin_panels.toml`](../../crates/geode-marketdata/src/core/builtin_panels.toml)).
A desk or user panel with a new name adds a kind. One with an existing name
replaces that panel whole: a `[cvi]` override that gives only a `title`
inherits nothing from the builtin and is refused for its missing keys.

```toml
[cvi_wide]
title = "CVI (wide)"
dataset = "cvi_params"
document = "cvi_params"
actions = ["marketdata::cvi_reanchor"]
value = { type = "f64", format = { precision = 6 } }
rows = { column = "term", identity = "date", label = "shown" }
columns = { axis = "node" }
header = [
  { column = "anchor_date", label = "anchor", type = "date" },
  { column = "spot_ref", label = "spot", type = "f64" },
]
slice = [
  { column = "forward", label = "fwd", format = { precision = 3 } },
  { column = "atm", label = "atm" },
  { column = "skew", label = "skew" },
]
```

| Key | Required | Meaning |
|---|---|---|
| `title` | yes | The tile title and badge text |
| `dataset` | yes | The document dataset the panel reads |
| `document` | yes | The registered document kind `:upload` serializes with |
| `value` | yes | `type` (`f64` or `i64`) and `format` of a pivot's grid values; a flat panel's fallback type |
| `rows` | yes | `column`, `identity` (`"minted"`, or `f64`, `i64`, `date`, `utf8` for a trader-named row), and `label` (`"shown"` or `"hidden"`) |
| `columns` | yes | Exactly one of `axis = "<column>"` (a pivot: one grid column per distinct axis value) or `values = [ … ]` (a flat table) |
| `header` | no | `[ { column, label, type } … ]`: document attributes shown and edited in the header, in this order |
| `slice` | no | `[ { column, label, format? } … ]`: per-slice values painted ahead of a pivot's grid |
| `actions` | no | Registered kind-action ids, in menu order |

Each flat value column is `{ column, label, type, required, format?,
choices? }`. `type` is `f64`, `i64`, `date`, or `utf8`; `required` (a
boolean) says whether an inserted row must fill the cell; `choices` is a
closed, non-empty list of distinct strings and applies to `utf8` only.
`choices` must match the document kind's own vocabulary for that column
(the dividend kind's `status`: `estimated`, `declared`, `paid`,
`cancelled`). Load does not check this, because a document kind does not
expose its vocabulary; a choice the kind does not know is accepted by the
editor and then refused by the kind's writer on every upload.

A `format` table accepts the view format keys `precision` (0–12),
`thousands`, `negative`, and `scale`, over a base of no places, no grouping,
minus signs, and no scaling. An `f64` value or column must state its
`precision`: the panel will not guess how many places a value carries. A
slice's `format` overlays the panel's `value.format`; a slice without one
takes it whole. `format` is refused on a `date` or `utf8` column. Panels
paint every value in the foreground, so `color` is an unknown key here, and
a malformed format value is an error rather than a view reader's warning.

### Refusals

The panel's name must be lower-case ASCII letters, digits, and `_`, starting
with a letter, and must not be another module's kind (`blotter`,
`timeseries`, `pricer`, `diagnostics`, `placeholder`). It must not end in a
placement suffix (`_horizontal`, `_vertical`, `_stacked`): the shell spells
each kind's add-tile placements `tile::add_<kind>_<placement>`, so
`[cvi_stacked]` would claim the `cvi` panel's stack action. Beyond that, a panel
is refused with one Error at `panels.<name>[.<field>]` when:

- a key is unknown, a required key is missing, or a value has the wrong
  shape or type, including a malformed `format` and an `f64` without a
  `precision`;
- its `dataset` is not declared, or is not a document dataset (reported at
  `.document`, where the document kind is matched against it);
- its `document` names no registered document kind, or a kind whose columns
  and types do not match the dataset's document columns;
- a named column is missing from the dataset, has the wrong role (a header
  column must be a document attribute; a value, flat column, or slice must be
  a value column), or has a type other than the one the panel states;
- the row axis is not the dataset's first axis, or the panel does not lay out
  exactly the dataset's axes (the row axis alone for a flat panel, the row
  axis then `columns.axis` for a pivot);
- a minted row identity sits on a non-`utf8` axis, or a typed identity has a
  hidden label (a new row is named in the row-label column);
- a pivot does not leave exactly one value column unnamed for its grid, or
  that column's type is not `value.type`;
- a slice column is not `f64`, or its type differs from `value.type` (slices
  are edited as the panel's value type and read and uploaded as `f64`), or a
  flat panel has slices;
- two flat columns, or two slices, share a label, or a slice label could
  also be a pivot column label (a number over a numeric axis, a
  `YYYY-MM-DD` date over a date axis);
- a pivot with slices has an axis of another type: its labels come from the
  data, so no slice label can be proven distinct at load;
- a column is named twice across `rows`, `columns`, `header`, and `slice`;
- a column the document kind writes is not named by the panel. The
  dataset's document key is exempt, because the tile supplies it, and so is
  a pivot's one unnamed value column, which is its grid;
- the `header`, the flat `columns.values`, or a pivot's values (its grid
  column, then each `slice`) do not follow the document kind's column order.
  Upload emits them in the panel's order and the kind's writer refuses any
  other, so a reordered panel would load and paint but never upload;
- an action id is not registered, or is listed twice.

A refused panel is not a tile kind: it has no add-tile action and no picker
row, the status bar's config error count includes it, and the diagnostics
tile names its path. A saved tile of a refused kind restores as a
placeholder with its session record kept, so a fix and a restart bring it
back. These Errors stay in the config section across hot reloads and do not
block one.

Any panel over an undeclared dataset is refused, the builtin ones included.
`cvi_params` and `dividend_schedule` are declared only by the `--demo`
layer, so a configuration that has a `views` document but no market-data
datasets starts with `config 2 errors`. No layer can remove a builtin panel
— an override of the same name must itself be a valid panel — so clear
them by declaring both document datasets with the same key, axes, columns,
roles, and types as
[`examples/demo-config/datasets.toml`](../../examples/demo-config/datasets.toml).
The document kinds and the panels check every column, so a declaration that
differs in any of these leaves the panel refused with a different Error.
Declared that way, the panels are accepted and show nothing until a source
publishes their documents. Without any `views` document no data module
starts, panels included, and no panel is checked.

### Restart and kind actions

`panels` is restart-required: the running panel kinds, their specs, and
their Errors are fixed at startup. An edit sets the status bar's restart
indicator (`panels changed — restart to apply`) and changes no open panel;
returning the documents to their startup contents clears it.

A kind action is registered code, not configuration. `geode-app` registers
each action's id, title, and whether it is built; a panel offers any
registered action by id. The builtin set is CVI's
`marketdata::cvi_reanchor` and `marketdata::cvi_recalc_forward`, both
unbuilt: their menu rows are disabled and the tile answers `not built yet`.
Adding an action means adding its handler and registration.

## Scenario shapes

`scenarios.toml` defines the scenario tile kinds. Each top-level table is one
shape and its name is the tile kind: `[vega_kt]` is the `vega_kt` kind, its
add-tile actions `tile::add_vega_kt` and the placement variants, its palette
rows `Vega K/T: Split` and so on (the shape's `title`), its picker row
`Vega K/T`, and its session tiles carry `module = "vega_kt"`. The builtin
layer ships five
([`builtin_scenarios.toml`](../../crates/geode-scenario/src/core/builtin_scenarios.toml)):
`vega_kt`, `vega_spot`, `spot_ladder`, `by_maturity` and `rho`. A desk or
user shape with a new name adds a kind; one with an existing name replaces
that shape whole, so a `[vega_kt]` override giving only a `title` inherits
nothing and is refused for its missing keys — and the builtin it replaced is
gone with it. The document is restart-required: a reload reports
`scenarios changed — restart to apply` in the status bar and the running
kinds stay. Each accepted shape also registers one grouped view
`scenario:<kind>` with the data service (never with the blotter's view
list); a configured view of that name refuses the shape at startup, and one
added by a later reload is dropped with a Warning (see
[Hot reload](#hot-reload)). [Scenario tiles](features.md#scenario-tiles)
describes the tile itself.

```toml
[vega_kt]
title = "Vega K/T"
dataset = "vega_buckets"
rows = "tenor"
columns = { axis = "moneyness" }
value = "vega"
paint = "heat"

[spot_ladder]
title = "Spot ladder"
dataset = "spot_ladder"
rows = "spot_shift"
columns = { measures = [
  { column = "npv", label = "P&L vs base" },
  { column = "delta", label = "Δ01", format = { precision = 0, scale = "k" } },
  { column = "theta", label = "Θ", unit = "per day" },
] }
diff = { base = 0, measures = ["npv"] }
paint = "heat"
```

| Key | Required | Meaning |
|---|---|---|
| `title` | yes | The palette and header name (`{title}: Split` in the palette, `{title}` in the picker, the tile header and the row menu's `Open {title}`) |
| `dataset` | yes | A measures dataset that declares `axes` |
| `rows` | yes | The axis down the side |
| `columns` | yes | Exactly one of `axis = "x"` or `axis = ["x", "y"]` (a matrix; two axes make a two-level header, outer first) or `measures = [ … ]` (a ladder: one column per measure) |
| `value` | with an axis `columns` | The measure a matrix paints; refused with a measures `columns` |
| `format`, `unit` | no, with `value` only | The painted measure's format and the text under its header |
| `diff` | no, with a measures `columns` only | `{ base, measures }`: the listed measures paint value minus the base row's value, the base row stays absolute and is highlighted |
| `paint` | yes | `heat` or `plain`, the fill a tile starts with |

Each ladder measure is `{ column, label?, format?, unit? }`; `label` defaults
to the column name. A `format` table takes the view format keys `precision`,
`thousands`, `negative`, `scale` and `color` over the measure default (two
places, grouping, minus signs, sign color). A measure `m` whose dataset also
declares `m_usd` as a measure has a currency alternate the tile toggles with
`c`; one without has none. The tile starts in USD when any painted measure
has a twin and `c` toggles; a shape with no twin starts native and `c`
refuses with `no USD twin for <measure>`. `p` cycles the paint, `m` toggles
diff on a shape that has one, `:scale k|m|1` overrides every column's scale
or restores each column's own.

### Refusals

The name must be a usable tile kind (lower-case ASCII letters, digits and
`_`, starting with a letter) and must not end in a placement suffix
(`_horizontal`, `_vertical`, `_stacked`). Beyond that a shape is refused with
one Error at `scenarios.<name>[.<field>]`, attributed to the layer that
supplied it, when:

- a key is unknown, a required key is missing, or a value has the wrong
  shape, including a malformed `format`;
- `columns` gives both or neither of `axis` and `measures`, an empty
  `axis = []`, more than two axes, the same axis twice, an empty axis name,
  or the `rows` axis;
- a ladder's `measures = []` is empty;
- `value` is missing with an axis `columns`, or present with a measures
  `columns`; `format` or `unit` sits at the top level of a ladder;
- a ladder lists a measure twice or two measures share a label;
- `diff` accompanies an axis `columns`, lacks `base` or `measures`, has an
  empty `measures = []`, lists a measure not in `columns` or twice, or its
  `base` is neither a finite number nor a string;
- `paint` is anything but `heat` or `plain`;
- `dataset` is not declared, or is not a measures dataset with `axes` (a
  builtin shape over an undeclared dataset — every builtin shape when the
  demo layer is absent — is instead withheld without an Error: it logs at
  info level and its kind does not appear);
- `rows` or a `columns` axis is not an axis of the dataset, or an axis of
  the dataset is laid out nowhere (it would be summed across silently);
- `value` or a ladder measure is not a measure of the dataset;
- `diff.base` is a number on a text axis, or text on a numeric axis;
- a measure's `_usd` twin sits at another grain than the measure;
- the name is another module's tile kind or an accepted panel's (panels load
  first);
- the view `scenario:<name>` is already configured (the message asks to
  remove it and restart).

Datasets are checked against the assembled schema, so a desk shape over a
desk dataset is accepted when both documents agree. A refused shape never
becomes a tile kind.

## Runtime edits

The application writes only the user layer. Desk configuration is shared and
builtin configuration is compiled, so neither is a valid target for an
interactive edit.

All shell writes pass through `geode_shell::config_write`. `submit` accepts an
operation synchronously and runs it on the background executor. Submissions
to the same configured directory retain acceptance order. Dropping the result
task does not cancel an accepted write; process exit remains best-effort.

`edit` and `try_edit` hold a directory transaction lock from reading and
parsing the current file through mutation and replacement. Parse failure,
mutation failure, or mutation unwind leaves the original file untouched.
New documents receive the current version stamp. `write` instead replaces a
whole document without parsing or preserving its previous contents.

Replacement writes a uniquely named temporary file beside the target, syncs
it, then renames it over the target. Temporary names end in `.tmp`, preventing
the poll from reading partial documents. The directory is not fsynced, and an
error can leave a temporary file behind. Ordering coordinates this process's
writers using the same directory path, not other processes or symlink aliases.

Dialogs edit typed drafts rather than TOML text. The draft owns validation and
dirty state; a shared `InputState` is only the active field editor.
`sync_dialog_text` moves text and focus between them. Object dialogs queue
valid drafts without a Save action, apply them to the shell after a debounce,
then persist to the user layer. Memory acceptance and disk success are
separate outcomes. See [configuration dialogs](configuration-dialogs.md) for
inherited objects, presentation routing, reload interaction, and write failures.

Modules write configuration only through the shell's config door
(`FrameRef::queue_config_edits`, see [the config
door](shell.md#the-config-door)), which joins the object dialogs' pending
batch. `dimensions` has a runtime writer this way, the Classifications tile
(see [features](features.md#classifications)): a classification edited in a
tile is written as its whole `dimensions.toml` object, never as one changed
key, because the document replaces whole named objects across layers. Editing
a desk or builtin classification therefore forks it: the user layer receives
the complete object, which shadows the lower layer's from then on, including
later desk changes to it. Removing the object removes only the user layer's
copy, which reveals the lower layer's again; it cannot delete a desk
classification.

An inherited object can be edited by creating a user override. Deleting that
override reveals the lower-layer value again. `overrides.toml` records accepted
schema or source drift where a dialog must distinguish a deliberate exception
from an accidental mismatch.

## Hot reload

The watcher waits 500 ms between polls, then scans desk and user directories
for immediate `*.toml` children, excluding `session.toml`. It compares paths
and modification times, so additions and removals trigger reload but an edit
with an unchanged mtime does not. Scan failures are silently skipped and may
look like removals. Scanning and loading run in the background; validation
and runtime application run on the UI thread. Work adds to the poll interval.
The watcher is the app's (`ShellApp::spawn_watcher`): one loop however many
windows are open, started with the app and ended once every registered
window has closed, even while the quit hooks still hold the app entity or
the bridge's closures a window's view. No tick (config scan, session write,
memory sample, date refresh) runs for nobody.

The first poll establishes a baseline without loading; an edit between startup
loading and that poll can therefore go unnoticed until another file change.
Each changed snapshot becomes the new baseline before validation, so rejected
input is retried only after another detected change. Unchanged polls retain
the last reload outcome.

A candidate reuses all original builtin documents, including demo defaults.
Checked module keymap fragments are inserted between builtin and desk/user
keymaps. File-load, modifier-alias, clock, and assembled-keymap diagnostics
reach the rejection decision. Any error there retains the active `Config`
and runtime settings; warnings allow application. Both outcomes replace the
current config-diagnostics batch, and rejection also logs and emits its errors.
Compiled-fragment diagnostics remain visible but do not block reload because
their invalid bindings were already dropped.

This decision does not validate every typed document. Grouping, saved-scope,
log-level, theme, and bridge view readers run later; their problems do not
roll back the whole reload. Grouping and scope diagnostics are logged;
theme-application warnings are currently discarded. Keep-last-good therefore
applies to the errors collected before the decision, not every later reader.

Accepted candidates update runtime state according to their inputs:

| Input | Live effect |
|---|---|
| Keymap layers or modifier alias | Replace bindings and close every window's open palette, whose snapshot depends on them; each window restores focus on its next render |
| Effective `app.theme` | Apply the theme when changed; unrelated edits preserve the current theme |
| `groupings`, `datasets`, or `dimensions` | Rebuild shared grouping slots |
| `scopes`, `datasets`, or `dimensions` | Rebuild saved scopes |
| `expressions`, `datasets`, or `dimensions` | Rebuild named expressions; a changed or redefined entry bumps the frame's config version so a tile whose scope references it requeries |
| `datasets` or `dimensions` | Rebuild dimension-picker columns |
| Views, either presentation document, dimensions, colors, value colors, watchlists, scopes, or expressions | Emit `ConfigReloaded` for the app bridge |
| `app` | Emit `AppSettingsReloaded`; the bridge hands `blotter.stale_after` to the blotter, panel and scenario factories |
| `app.volwatch` (`refresh`, `vega`, `stale_after`) or `vol_views` | The VolMonitor's own revision observer re-reads `[volwatch]` and `vol_views.toml`, resolves the tabs against the startup schema and pushes the settings whole to the factory; an unchanged key or document neither pushes nor repeats a warning |
| Sources, datasets, egress, positions, panels, scenarios, `app.pricing.adapter`, or `app.vol.model` differing from startup | Mark restart required; return to the startup inputs to clear it |

Document-change checks compare the original per-layer documents, including
their paths, rather than just merged values. Source and dataset changes can
therefore update shell presentation while the data engine still needs restart.
Pricing refresh is live and does not itself require restart.

Every accepted reload advances the frame's config revision and republishes
`Chords`. `UiSettings`, `SeriesSettings`, and `AppClock` are published only
when their values change. The view-related event is queued before any frame
notification so the bridge refreshes factory/handle views before tiles observe
the new revision and submit queries. `AppSettingsReloaded` is queued the same
way, so a tile's frame flip re-arms its stale wake-up against the new
threshold. `Reloaded` follows both, carrying the revision and the reload's
"changed" flags, so each window's own work (its picker vocabulary, its
palette, its dialogs) runs after the bridge's and before the frame notifies;
a rejected reload emits `ReloadRejected`, on which each window repaints its
status.

View and derived-dimension replacements use a latest-value mailbox into the
data service, so a full request queue cannot permanently lose a configuration
reload.

The bridge's `ConfigReloaded` handler runs for changes to views, view/dataset
presentation, dimensions, colors, value colors, watchlists, scopes, or
expressions. It uses the same presentation-aware view loader as startup,
updates module factories, offers views/dimensions to the service, and
re-folds the watchlists against the startup schema and the reloaded saved
scopes and named expressions (see [the watchlist cache](shell.md#state-ownership)). The colors it hands the blotter and
timeseries factories carry the `value_colors` mapping, checked against those
definitions and the declared dimensions, as startup's do. The scenario
factories share one schema, dimensions and colors cell the handler rewrites,
so a reloaded color reaches an open scenario tile with its next picture (the
requery the reload's config revision triggers). A reloaded view named like a
running shape's `scenario:<kind>` view is dropped before any consumer sees
it, with one Warning at `views.<name>`: the derived view keeps answering
until a restart, which refuses the shape instead. This is not an
atomic update across factories and workers; the handle acknowledges
retention, not application. See
[view replacement](request-delivery.md#view-replacement-and-shutdown).

That handler also rereads the factory validation schema. Dataset edits
require restart, so a later eligible reload can update the factory schema
before the running service is rebuilt. The stale threshold lives in `app` and
reaches the blotter, panel and scenario factories through
`AppSettingsReloaded`, on its own; the pricer reads it in its own revision observer. That observer's key
also covers `colors` and `value_colors`, so an edit to either alone re-runs
the pricer's reload and hands its factory colors carrying the mapping; their
diagnostics are the `ConfigReloaded` handler's to report. Presentation and
color-reader (including value-color) diagnostics append to the retained
data-diagnostics lane; the shell remains responsible for replacing the
current config-diagnostics batch.

## Keymaps and actions

Action IDs are the stable vocabulary shared by configuration, the command
palette, tooltips, and dispatch. Builtin bindings load first, module fragments
extend them, then desk and user entries override or unbind them. The keymap
compiler consumes original layer documents rather than the generic merged
array. Context predicates make bindings conditional; among matching exact
sequences, the last binding wins without a separate specificity priority.

The primary `mod` alias defaults to Alt and may be set to Command. `ctrl` is
refused as the alias because it collides with shipped literal Control
bindings. Invalid entries are diagnosed and skipped; unknown actions warn.
Compilation errors participate in reload rejection. Parsing a key name does
not establish whether a platform can deliver it. See [keymaps and actions](keymaps.md)
for syntax, sequences, counts, module restrictions, and edit/reset behavior.

## Theme, time, and logging

A theme name selects a complete light or dark theme; there is no independent
mode toggle. Named data colors resolve from theme anchors in OKLCH and seek
a 3:1 contrast ratio. Custom themes can prevent the available lightness range
from reaching that target; untinted semantic tokens retain their exact color.
See [color resolution](typed-documents.md#colors-and-numeric-formatting).

The bundled choices include Kanagawa Wave, Dragon, and Lotus; Rosé Pine,
Rosé Pine Moon, and Rosé Pine Dawn; and GitHub Light and GitHub Dark Dimmed.
Use the complete displayed name in `[theme] name`, Settings, or the command
palette. Lotus, Dawn, and GitHub Light are light themes. Theme assets and
upstream attribution are documented in [the theme catalogue](../../assets/themes/README.md).

Default chart series use an explicit five-color palette chosen for each named
variant. Bundled palettes are checked for at least 3:1 background contrast
and separation between all five resolved colors. Chip and control text uses
a separate 4.5:1 floor against its actual fill, including hover and pressed
states. Colors that already meet the text floor stay unchanged.

`[time]` configures the trader-facing IANA time zone and start/end-of-day
presets. Displayed times use `geode_core::clock::Clock`; crates do not read
`chrono::Local` directly. Log and crash filenames still roll by UTC date so
filesystem naming is stable across configured display zones.

`[log]` controls the `geode::*` target levels, keyed by suffix: `ingest`,
`query`, `config`, `session`, `shell`, `theme`, `pricing`, `vol`, `memory`,
and `collector` (`geode::collector`, the background collector's own
lifecycle), plus `default`. Third-party targets remain capped at `warn`.
Runtime level changes persist through the same ordered user configuration
write path. The background collector applies the same `[log]` levels to its
own file (`collector.YYYY-MM-DD.log`) each time it takes the store; a
change reaches it then, not live.

## Background collector

The background collector (`geode-collector`) reads the same layers as the
app, from the same directories: the builtin data layer (the app's
`pricer_sheets` and `pricer` declarations, plus the `--demo` layer), the
desk layer, and the user layer. It lacks only the app's UI documents, which
decide nothing about the store, so both processes build the same schema
and sources; a `geode-app` test holds them to it. It reads the layers
afresh each time it takes the store and has no hot reload: an edit made
while it holds the store takes effect at its next acquire (after the app
next opens and closes), its restart, or login. A changed `data.db_path`
takes effect only at its restart, because its lease is on the store it
started with; it warns at each acquire while the configuration names
another.

`[collector] memory_limit` in `app.toml` is an opt-in DuckDB
`memory_limit` (such as `"2GB"`) for the collector's writer; the app never
reads it. Unset, the default, the collector leaves DuckDB's own limit, as
the app does. A finite 512MB limit made DuckDB abort the collector with an
internal assertion during large CSV loads, and a service manager
restarting it into the same load could crash-loop, so no default is set
until an overnight footprint measurement chooses one. A value that is not
a number and a unit (`B`, `KB`, `MB`, `GB`, `TB`, `KiB`, `MiB`, `GiB`,
`TiB`, any case) warns at `app.collector.memory_limit` and is ignored.

`[links] include_tile_filter` decides whether a link-group emitter's own
`:filter` layer is composed into the scope it posts (base, then layer, then
cursor path; see [link groups](shell.md#link-groups)). It is a boolean, on
by default, so a follower shows the rows under the emitter's cursor as the
emitter shows them; off, a tile filter stays local to its tile and followers
see the emitter's base and path alone. A missing key or a non-boolean value
reads as on, without a diagnostic. The settings dialog's `Linked scope
includes tile filter` row (category Links, `On` / `Off`) steps it and
persists it to the user layer through `config_write`; a failed write logs
a `geode::config` warning and keeps the change for the session. A hot reload
applies a changed value. Either way every emitter is re-pulled at once:
the rule moves no frame version, so the ordinary re-pull would not notice
it.

## Pricing

`[pricing]` in `app.toml` configures the line pricer. `adapter` names the
pricer built into the binary; a change needs a restart. `refresh` is the
pricer tile's default reprice interval: `30s` when absent, `"off"` to disable
the timer, or a positive duration string accepted by the shared duration
parser. Zero, invalid strings, and non-string values warn at
`app.pricing.refresh` and use 30 seconds. A reload applies `refresh` to every
open pricer tile without a restart; a sheet's own `:refresh` still overrides
it.

`underlyings` lists the underlyings the pricer's entry bar suggests, in the
order it offers them: an array of strings, trimmed and upper-cased, with
blank entries dropped and duplicates keeping their first position. A non-string element warns at
`app.pricing.underlyings` and is skipped. A value that is not an array warns
and is ignored: on a reload the bar keeps the list it had. A reload applies a
valid list to open tiles without a restart. An edit to `underlyings` alone
still runs the pricer's full reload, which restarts every open tile's refresh
timer. An absent `underlyings` clears the list, including on reload; a
non-array value at startup leaves it empty. With an empty list the bar says
no underlyings are configured.

`payout_currency = "<dataset>.<column>"` names the reference column a new
pricer line's payout currency defaults from, looked up by the line's
underlying. When the key is absent it is `underlyings.currency` if the
`underlyings` reference dataset is keyed by one column and declares a
`currency` column of type `utf8`, and otherwise nothing, without a
diagnostic. An explicit value must split on one `.` into a declared
reference dataset keyed by exactly one column and one of its non-key `utf8`
columns. A multi-column key never matches: a lookup joins its parts with
`/`, and the pricer looks up by the underlying alone. A column of another
type never parses as a currency code. Anything else, including a
non-string value, is an error at `app.pricing.payout_currency`
and resolves to nothing, so new lines get no currency rather than one read
from a guessed column. The value is checked against the startup schema, the
one the running service serves, because datasets are restart-required. A
reload applies the setting without a restart, and a change to it alone
passes the pricer's reload gate. Unlike `underlyings`, an invalid value on a
reload does not keep the last good setting: it drops the running payout
source, so newly typed lines read `needs currency` until the value is
fixed. Lines that already have a currency keep it, and blank lines refill
when a reload names a different valid source.

The pricer's views are ordinary `views.toml` views whose `dataset` is
`pricer`. The builtin layer carries `vanilla` and `barrier`; a desk or user
entry replaces a whole view by name, and `view_presentation.toml` and
`dataset_presentation.toml` apply as they do to any view. A pricer view may
not declare `joins` or a `derived` column: either is an error diagnostic on
the view, and the pricer drops that view rather than paint part of it. A
pricer view's `grouping` is honoured: it is the tile's chain when neither a
`:group` pin nor the frame's active slot gives one (see
[grouping](features.md#grouping)). A
`pricer_views` document is no longer read; its presence is an error naming
`views.toml`. A reload that changes a view, either overlay or the colors
reaches open pricer tiles; a tile whose view disappeared shows the first
`pricer` view with a header notice, and a view whose every column is hidden
says so in the header.

`pricer_templates` holds the package templates the pricer's shorthand
accepts. The builtin layer carries seven: `CS`, `PS`, `STRD`, `STRG`, `RR`,
`FLY`, and `CAL`. Each entry is a table of legs; a leg names its signed
`weight`, which typed `strike` it takes, which typed `expiry` it takes, and
its option `kind`:

```toml
[CONDOR]
legs = [
  { weight = 1, strike = 1, kind = "C" },
  { weight = -1, strike = 2, kind = "C" },
  { weight = -1, strike = 3, kind = "C" },
  { weight = 1, strike = 4, kind = "C" },
]
```

`SPX Z26 4800/4900/5100/5200 CONDOR` then prices those four legs. `strike`
and `expiry` are 1-based indices into the strikes and expiries typed in the
shorthand; `expiry` defaults to 1, so only a multi-expiry template such as
`CAL` names it. The number of strikes and expiries a template takes is the
highest index its legs use.

A name is 1 to 8 letters or digits, a letter first, and is case-insensitive:
`condor` and `CONDOR` are the same template. `C`, `P`, and `CUSTOM` are
reserved. Each entry is validated alone, and a bad one is dropped with an
error diagnostic at its path while the rest load. An entry is bad when its
name is invalid or reserved, it is not a table, `legs` is missing or has
fewer than two legs, a leg is not a table, a `weight` is zero or not an
integer, a `strike` or `expiry` is below 1 or above the number of legs,
`kind` is not `"C"` or `"P"` (either case), or the strike or expiry numbers skip one (a
template using strikes 1 and 3 has no strike 2). A dropped entry whose
name had a definition keeps that previous definition, with a warning: the
running one on a reload and the built-in one at startup, so a typo in a
desk `RR` never makes `RR` disappear. Unknown keys, on the entry
or on a leg, warn and are ignored. Two spellings of one name in the same
merged document keep the later entry, in the earlier one's position, with a
warning. When the later spelling is invalid, the name keeps its previous
definition instead (at startup, its built-in one); only a name with no such
definition falls back to the earlier spelling.

Like other named objects, an entry in a higher layer replaces a lower-layer
entry of the same name, including a built-in: a desk `RR` redefines the risk
reversal for every sheet. A reload reaches open pricer tiles without a
restart. Rows already on a sheet keep their legs and prices; see
[the pricer](features.md) for how a package prints once its template is
removed or redefined.

## Vol

`[vol]` in `app.toml` selects the vol-surface evaluator the data service's
vol slice requests use. `model` names a model the binary registers; the
default and the only one in every build is `demo`, a smooth stand-in that is
not a financial model. A name the binary lacks warns at startup and every vol
slice answers with that reason. Changing `model` marks restart required, as `[pricing] adapter` does.

## VolMonitor

`[volwatch]` in `app.toml` configures the VolMonitor tile (see
[VolMonitor](features.md#volmonitor)) and `vol_views.toml` declares
its tabs. All three `[volwatch]` keys and the tabs are hot: a reload that
changes any of them pushes the new settings whole to the factory and the
open tiles; a `stale_after` change re-arms each tile's stale wake-ups; a
changed document replaces each tile's tabs, keeping the shown one by name.
The tile does not read `[blotter] stale_after`.

The tile reads three history datasets by fixed name: `vol_surface`,
`realized_vol` and `varswap`, and any further `history` dataset a tab's
open-form column names. A deployment that shows the tile declares the
three as history datasets of the shapes the board reads; while any one is
undeclared or misshaped, the data service refuses every board and the tile
shows `surface: <error>`. There is no setting to rename them. A dataset's
`history` window bounds the lens windows: the surface, realized, varswap
and any table dataset must declare at least the longest window the tile
offers (`2Y`), else a `2Y` z or percentile is computed over whatever
shorter history the dataset holds under the `2Y` label; the demo declares
`800d`.

- `refresh` is a duration or `"off"`; the default is `"60s"`. Below the 10s
  minimum it is raised to 10s with a warning; a value that is neither a
  duration nor `"off"` warns and uses 60s.
- `vega = { view, column }` names a grouped view over the risk dataset
  (grouped by `underlying_ref`) and the measure column the tile joins by
  underlying as book vega. Anything but two strings warns and ignores the
  setting. A view that later fails to load or lacks the column drops the
  column with a standing warning naming the setting; the board keeps
  painting.
- `stale_after` is a non-zero duration; the default is `"24h"`. A surface
  publishes once a day, so a row is stale when a day has passed without
  one; the blotter's 15-minute threshold would mark every row stale each
  evening. A value that is not a duration, or is zero, warns
  (`[volwatch] stale_after = … is not a duration; using 24h`) and uses the
  default. A stale row keeps its numbers' colors and shows its name in the
  muted tone; the header's surface time turns stale with the oldest row.

`vol_views.toml` is one top-level table per view, in document order, each
with a `columns` array, and a top-level string `default` naming the
opening tab (without one the first view opens). Layers replace whole views
by name, so a user view appends after the builtin four (`vol`, `skew`,
`varswap`, `surface`) unless it replaces one of them, and a user file that
lists shipped tabs in another order moves them (the merge rule above). The
Vol views dialog (`e` on a VolMonitor tile) writes this file's user layer; its
writer rewrites each column from its pickers, so a hand edit inside a
`[[columns]]` table that the reader does not know, or a comment on one,
does not survive the next save, while a comment on a tab's table and any
other top-level key do. A `[[columns]]` table the reader refuses is the
exception: the dialog carries it as it was read and writes it back in its
place, so a save never deletes a column the reader only warned about.
`cells = "pair"` (the
default) lays each measure out as a level and a companion column under one
group head; `cells = "single"` as one number per cell. A column is exactly
one of `measure = "<text>"` (with an optional `label` for the sub-head,
`group` for the group head, `level = "plain" | "heat" | "tint"` for the
level cell's paint and `companion = "chip" | "bar" | "slider"` for the
companion's; absent means the view's default), `spark = "<measure text>"`
with `span` (a duration; default `30d`), or `kind = "vega"` (book vega,
painted only with `[volwatch] vega` set). The measure text is one of the
ten built-in forms
(`point <tenor> <delta>`, `atfm <tenor>`, `forward <tenor>`, `skew
<tenor>`, `wing skew <tenor>`, `slope <long>-<short>`, `realized <window>`,
`iv-rv <tenor>/<window>`, `varswap <tenor>`, `vs spread <tenor>`) or the
open form `<dataset> [<k1>] [<k2>]`, which names a `history` dataset keyed
by `underlying_ref` first with exactly one value column and supplies its
remaining key values in key order (`ssr 3M`); the grammar and the
resolver's rules are in [typed documents](typed-documents.md#vol-views).
Every problem warns at its key (`vol_views.<view>.columns.<n>`): a column
that does not read or resolve drops alone, a view left with no column is
kept as an empty tab with a warning, a second view named as an earlier
one in any case is skipped, a `default` naming no view falls to the first,
and a document that leaves no valid view at all warns `vol_views: no valid
view; the tile shows the builtin tabs` and the builtin document stands in.
Resolution runs in the app against the schema the data service started
with, as watchlist rules do, so a dataset added since launch needs a
restart before a column may paint; the dialog accepts such a column with
the warning `<dataset> needs a restart to paint`.

```toml
config_version = 1
default = "vol"

[ssr]                      # a fifth tab beside the builtin four
[[ssr.columns]]
measure = "ssr 3M"         # the demo's precomputed skew-stickiness ratio
[[ssr.columns]]
measure = "atfm 3M"
label = "3m vol"
level = "heat"             # tinted against the column's levels
companion = "bar"          # a bar from zero in place of the chip
[[ssr.columns]]
spark = "atfm 3M"
span = "30d"
[[ssr.columns]]
kind = "vega"
```

## Maintaining configuration

When adding or changing a setting:

1. define and validate it in the owning typed document;
2. preserve provenance and last-good behavior;
3. decide whether it applies live or requires restart;
4. route interactive persistence through `config_write`;
5. update the relevant current guide and example configuration;
6. test merge, invalid input, user override, and reload behavior at the lowest
   layer that proves the contract.

Do not add a second parser or write path in a feature crate. A setting shared
with modules should be delivered through an explicit typed value, entity, or
genuinely application-wide GPUI global.
