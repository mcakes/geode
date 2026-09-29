# Configuration

Geode treats configuration as product data. Views, schemas, sources, keymaps,
themes, saved scopes, and presentation choices are readable TOML documents
that can be shared at desk level and overridden by one user.

## Layers and provenance

Configuration is loaded in three layers, from lowest to highest precedence:

1. **Builtin** defaults compiled into the application.
2. **Desk** configuration from `$GEODE_DESK_CONFIG`.
3. **User** configuration from `%APPDATA%\geode` on Windows or
   `$HOME/.config/geode` elsewhere.

Tables merge recursively. Scalars, arrays, and values of a different type
replace the lower-layer value at the same path; unrelated values remain.
Provenance records the winning layer at a leaf or whole-object root, and
`Config::explain` checks the requested path and then its recorded ancestors.
TOML order is preserved throughout the workspace because column and row order
are part of several document contracts.

Top-level entries in `views`, `view_presentation`, `dataset_presentation`,
`layouts`, `groupings`, `scopes`, `datasets`, `sources`, `egress`, `dimensions`,
`colors`, `expressions`, `pricer_templates`, `panels`, and `overrides`
replace whole named objects.
Overriding one source therefore requires its complete configuration, including
required fields; omitted fields do not inherit from the lower-layer source.

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
| `app.toml` | Theme, primary modifier, UI settings, logging, time zone, pricing and timeseries settings |
| `datasets.toml` | Dataset families, columns, roles, types, grains, retention, local publication, and `computed` |
| `views.toml` | Queryable views, joins, columns, expressions, grouping, and sorting |
| `sources.toml` | File, subscription, and fetch sources with readiness and adapter settings |
| `egress.toml` | Upload targets: adapter and a per-document address template |
| `panels.toml` | Market-data panels: the dataset, document kind, layout, formats, and kind actions of each panel tile kind |
| `dimensions.toml` | Derived dimensions used for grouping and scope |
| `groupings.toml` | The nine shared grouping slots |
| `scopes.toml` | Named scopes |
| `expressions.toml` | Named scope expressions, referenced by name from a saved scope or the frame |
| `colors.toml` | Named semantic data colors |
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

The schema declares meaning rather than storage details. Measure columns name
their aggregation grain. Document columns distinguish keys, attributes, and
typed values. Series datasets describe identities and time/value columns.
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
is loaded; both the frame's expression dialogs and the Scopes object dialog's
`expression` field additionally check a typed draft at Enter, so an unknown
column or a disallowed operator on a derived dimension is refused with the
field still open rather than accepted and left to fail later at query time
(see [input-and-dialogs.md's Frame expression](input-and-dialogs.md#frame-expression)
and [configuration-dialogs.md's Scope expression field](configuration-dialogs.md#scope-expression-field)).
Source adapter names, document kinds, module keymap fragments, pricer
names, and market-data panels depend on what the assembled application has
registered, so `geode-app` performs those cross-crate checks. Panels are
checked at startup only (they are
[restart-required](#market-data-panels)); the rest at startup and reload.

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

Schema changes do not migrate an existing DuckDB database. `apply_schema`
creates missing tables and columns needed by its own metadata, while payload
publication remains positional. Rebuild a demo database after changing column
membership, roles, grains, or order. A production schema migration must be an
explicit operation.

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

The `pricer` declaration marks its nine utf8 dimensions `textual = true`:
`sheet`, `template`, `underlying_ref`, `expiry`, `option_type`, `currency`,
`barrier_type`, `priced_at` and `status`. Its keys (`position_ref`,
`instrument_ref`) are not textual. The frame's text filter searches exactly
these columns on a pricer sheet, as it searches a stored dataset's textual
columns (see [the evaluator's parity contract](data-path.md#queries-and-time-travel)).
Derived dimensions in `dimensions.toml` apply to the pricer's scope as to a
blotter's: a `dimensions` edit alone reloads the pricer, and the reload hands
its factory the new dimensions before every open tile re-applies its scope.

## Source configuration

`sources.toml` has one top-level table per source, such as `[risk_files]`.
Every source needs a declared, non-local dataset. The `adapter` setting and
dataset family select its runtime path:

| Shape | Selection and required settings |
|---|---|
| Directory | `adapter = "csv_dir"` (the default), with `paths` globs. A series dataset is rejected. |
| Subscription | Another adapter over a document dataset, with a `document` kind and nonempty `topics`. |
| Fetch | Another adapter over a series dataset. No document kind or topic list is needed. |

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

Subscription defaults are `coalesce = "500ms"` and `source_time = "receive"`.
The coalescing interval spaces releases to ingest for each document key;
`"0"` disables coalescing, without guaranteeing message delivery or immediate
publication. `source_time = "document:<field>"` requires a document-level
`date` or `utf8` attribute. At receipt, a date becomes midnight UTC and a
string must parse as RFC 3339 with its offset. Schema validation cannot ensure
that a particular message supplies a valid value.

Topic patterns use `/`-separated levels. A whole `*` matches one level; a
final `>` matches one or more trailing levels. Empty patterns, empty pattern
levels, and a non-final `>` level reject the source. Other text is literal.
Directory-only settings warn and are ignored on adapter-backed sources;
subscription-only settings warn and are ignored on directory and fetch
sources. Parsed directory paths are cleared on adapter-backed sources.

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
cvi_params = "marketdata/cvi/{key}"
dividend_schedule = "marketdata/dividend/{key}"
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
| Keymap layers or modifier alias | Replace bindings and close an open palette whose snapshot depends on them; restore focus on the next render |
| Effective `app.theme` | Apply the theme when changed; unrelated edits preserve the current theme |
| `groupings`, `datasets`, or `dimensions` | Rebuild shared grouping slots |
| `scopes`, `datasets`, or `dimensions` | Rebuild saved scopes |
| `expressions`, `datasets`, or `dimensions` | Rebuild named expressions; a changed or redefined entry bumps the frame's config version so a tile whose scope references it requeries |
| `datasets` or `dimensions` | Rebuild dimension-picker columns |
| Views, either presentation document, dimensions, or colors | Emit `ConfigReloaded` for the app bridge |
| Sources, datasets, egress, panels, `app.pricing.adapter`, or `app.vol.model` differing from startup | Mark restart required; return to the startup inputs to clear it |

Document-change checks compare the original per-layer documents, including
their paths, rather than just merged values. Source and dataset changes can
therefore update shell presentation while the data engine still needs restart.
Pricing refresh is live and does not itself require restart.

Every accepted reload advances the frame's config revision and republishes
`Chords`. `UiSettings`, `SeriesSettings`, and `AppClock` are published only
when their values change. The view-related event is queued before any frame
notification so the bridge refreshes factory/handle views before tiles observe
the new revision and submit queries.

View and derived-dimension replacements use a latest-value mailbox into the
data service, so a full request queue cannot permanently lose a configuration
reload.

The bridge's `ConfigReloaded` handler runs for changes to views, view/dataset
presentation, dimensions, or colors. It uses the same presentation-aware
view loader as startup, updates module factories, and offers views/dimensions
to the service. This is not an atomic update across factories and workers;
the handle acknowledges retention, not application. See
[view replacement](request-delivery.md#view-replacement-and-shutdown).

That handler also rereads the stale threshold and factory validation schema.
A stale-threshold-only edit does not trigger it, and dataset edits require
restart. A later eligible reload can therefore update factory settings or
schema before the running service is rebuilt. Presentation and color-reader
diagnostics append to the retained data-diagnostics lane; the shell remains
responsible for replacing the current config-diagnostics batch.

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

Default chart series use an explicit five-color palette chosen for each named
variant. Bundled palettes are checked for at least 3:1 background contrast
and separation between all five resolved colors. Chip and control text uses
a separate 4.5:1 floor against its actual fill, including hover and pressed
states. Colors that already meet the text floor stay unchanged.

`[time]` configures the trader-facing IANA time zone and start/end-of-day
presets. Displayed times use `geode_core::clock::Clock`; crates do not read
`chrono::Local` directly. Log and crash filenames still roll by UTC date so
filesystem naming is stable across configured display zones.

`[log]` controls the `geode::*` target levels. Third-party targets remain
capped at `warn`. Runtime level changes persist through the same ordered user
configuration write path.

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

The pricer's views are ordinary `views.toml` views whose `dataset` is
`pricer`. The builtin layer carries `vanilla` and `barrier`; a desk or user
entry replaces a whole view by name, and `view_presentation.toml` and
`dataset_presentation.toml` apply as they do to any view. A pricer view may
not declare `joins` or a `derived` column: either is an error diagnostic on
the view, and the pricer drops that view rather than paint part of it. A
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
