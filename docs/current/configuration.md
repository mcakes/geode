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

Tables merge recursively. A value in a higher layer replaces the value at the
same path; unrelated lower-layer values remain. Every merged value retains its
source layer so the UI can explain whether it is inherited or overridden.
TOML order is preserved throughout the workspace because column and row order
are part of several document contracts.

Named objects such as sources, datasets, and views are exceptions to recursive
merging: a higher layer replaces the entire table for that name. Overriding
one source therefore requires its complete configuration, including required
fields; omitted fields do not inherit from the lower-layer source.

Each document carries `config_version`. A missing version is diagnosed and
read as current where compatibility permits; an unsupported version is
rejected. Parse and validation failures become `Diagnostic` values instead of
panics. Reload keeps the last valid effective configuration.

## Documents

The main configuration documents have distinct owners:

| Document | Defines |
|---|---|
| `app.toml` | Theme, primary modifier, UI settings, logging, time zone, pricing and timeseries settings |
| `datasets.toml` | Dataset families, columns, roles, types, grains, retention, and local publication |
| `views.toml` | Queryable views, joins, columns, expressions, grouping, and sorting |
| `sources.toml` | File, subscription, and fetch sources with readiness and adapter settings |
| `dimensions.toml` | Derived dimensions used for grouping and scope |
| `groupings.toml` | The nine shared grouping slots |
| `scopes.toml` | Named scopes |
| `colours.toml` | Named semantic data colours |
| `dataset_presentation.toml` | Desk-level column presentation between schema and view overrides |
| `keymap.toml` | User bindings layered over builtin and module bindings |
| `overrides.toml` | Intentional drift accepted from the configuration dialogs |
| `session.toml` | Layout and transient session state; not part of the layered config merge |

The schema declares meaning rather than storage details. Measure columns name
their aggregation grain. Document columns distinguish keys, attributes, and
typed values. Series datasets describe identities and time/value columns.
Views refer to those declarations and may narrow presentation without changing
the underlying dataset.

## Validation boundaries

`geode-core` parses and validates configuration without I/O. It returns typed
documents plus diagnostics. Consumers derive their runtime state from those
types; modules do not reopen configuration files.

Scope expressions use a restricted grammar validated against the schema. They
are never raw SQL. Source adapter names, document kinds, module keymap
fragments, and pricer names depend on what the assembled application has
registered, so `geode-app` performs those cross-crate checks at startup and
reload.

Schema changes do not migrate an existing DuckDB database. `apply_schema`
creates missing tables and columns needed by its own metadata, while payload
publication remains positional. Rebuild a demo database after changing column
membership, roles, grains, or order. A production schema migration must be an
explicit operation.

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

## Runtime edits

The application writes only the user layer. Desk configuration is shared and
builtin configuration is compiled, so neither is a valid target for an
interactive edit.

All shell writes pass through `geode_shell::config_write`. A submission is
accepted synchronously, then the complete read, parse, edit, write, sync, and
rename operation runs on the background executor. Writes to the same directory
retain submission order. A parse failure leaves the original file untouched.
Temporary filenames do not end in `.toml`, preventing the reload poll from
seeing a partial document.

Dialogs edit typed drafts rather than TOML text. The draft owns validation and
dirty state; a shared `InputState` is only the active field editor.
`sync_dialog_text` moves text and focus between them. A successful save writes
the user layer and allows normal reload to install the result.

An inherited object can be edited by creating a user override. Deleting that
override reveals the lower-layer value again. `overrides.toml` records accepted
schema or source drift where a dialog must distinguish a deliberate exception
from an accidental mismatch.

## Hot reload

The reload worker polls configuration files. Polling avoids relying on file
watch semantics that differ across local and network filesystems. A decision
step compares the candidate configuration with the current valid state:

- valid changes replace the affected runtime state;
- invalid changes report diagnostics and retain the last valid state;
- unchanged values do not notify GPUI globals or rebuild module state;
- changes that cannot be applied safely at runtime are marked
  restart-required.

View and derived-dimension replacements use a latest-value mailbox into the
data service, so a full request queue cannot permanently lose a configuration
reload.

The bridge's `ConfigReloaded` handler runs for changes to views, view/dataset
presentation, dimensions, or colours. It uses the same presentation-aware
view loader as startup, updates module factories, and offers views/dimensions
to the service. This is not an atomic update across factories and workers;
the handle acknowledges retention, not application. See
[view replacement](request-delivery.md#view-replacement-and-shutdown).

That handler also rereads the stale threshold and factory validation schema.
A stale-threshold-only edit does not trigger it, and dataset edits require
restart. A later eligible reload can therefore update factory settings or
schema before the running service is rebuilt. Presentation and colour-reader
diagnostics append to the retained data-diagnostics lane; the shell remains
responsible for replacing the current config-diagnostics batch.

## Keymaps and actions

Action IDs are the stable vocabulary shared by configuration, the command
palette, tooltips, and dispatch. Builtin bindings load first, module fragments
extend them, and user entries override or unbind them. Context predicates keep
the same keystroke available to different focused surfaces.

The primary `mod` alias defaults to Alt and may be set to Command. `ctrl` is
refused as the alias because it collides with shipped literal Control
bindings. Binding syntax is validated as a whole assembled keymap so an
unspellable module binding is diagnosed before the window opens.

## Theme, time, and logging

A theme name selects a complete light or dark theme; there is no independent
mode toggle. Named data colours resolve from theme anchors in OKLCH and enforce
the readability floor described by their type.

`[time]` configures the trader-facing IANA time zone and start/end-of-day
presets. Displayed times use `geode_core::clock::Clock`; crates do not read
`chrono::Local` directly. Log and crash filenames still roll by UTC date so
filesystem naming is stable across configured display zones.

`[log]` controls the `geode::*` target levels. Third-party targets remain
capped at `warn`. Runtime level changes persist through the same ordered user
configuration write path.

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
