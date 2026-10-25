# Typed configuration documents

Merged TOML is input to several independent readers in `geode-core`. They
return typed values and diagnostics; a diagnostic does not universally mean
that the whole document was rejected. Some rules omit an object, some omit a
column, and others retain a value with a warning or clear one property.
Callers decide how to report and use those results. The
[configuration guide](configuration.md) describes layer precedence and the
reload acceptance gate, which does not run every typed reader.

## Dataset families and validation

[`SchemaSpec::from_doc`](../../crates/geode-core/src/schema/mod.rs) reads
`datasets.toml`. Its reserved `config_version` entry is skipped rather than
parsed as a dataset. An omitted family defaults to `measures`.

| Family | Shape and validation |
|---|---|
| `measures` | Columns declare roles and fixed grains. Measures and grain-bearing attributes create storage grains. An optional `axes` list names axis columns that extend the key of every grain holding a measure (below). Document-only roles and uncarriable dimensions are dropped. Missing grain-key declarations and reserved metadata names warn without removing the dataset. |
| `document` | A nonempty ordered key identifies a document; nonempty ordered axes identify its rows. Key columns must be grainless utf8 dimensions. At least one value is required. Invalid identity or row shape removes the dataset. |
| `series` | Storage implies `source`, `series_id`, `ts`, `received_at`, and `value`. Declared columns are dropped and nonempty key/axes arrays cleared with errors. Invalid retention windows report errors and become unbounded. |
| `reference` | One keyed table replaced whole per snapshot. A nonempty `key` of grainless utf8 dimensions identifies a row; every other column is a grainless attribute of utf8, f64, i64, date, or bool. `axes`, timestamp columns, and the storage names `batch`, `book`, `source_file_id`, `gen_id`, and `source_time` are refused. Any failure removes the dataset, because a half-understood mapping table would place rows wrongly. Storage order is the key columns, then the rest in declared order. A reference dataset offers no grouping vocabulary and is neither a document-kind nor an egress target. |
| `history` | Keyed timestamped rows, cached per key from an on-demand source. A nonempty `key` of distinct grainless utf8 or f64 dimensions identifies an observation's key tuple, and at least one f64 `value` column holds what was observed. No other role is accepted, every dimension must be in the key, and `axes`, timestamp columns, and the storage names `ts`, `received_at`, `source`, `batch`, `source_file_id`, `gen_id`, `source_time`, `from_ts`, `to_ts`, `ts_us`, and `rn` (the coverage, staging and read columns) are refused. Any failure removes the dataset: a key the cache cannot ask by would file rows under keys no request covers. `history` and `retention` windows are read as for series. Storage order is the key columns, `ts`, `received_at`, then the values. A history dataset offers no grouping vocabulary, and a view naming one is refused with an error saying it is a history dataset: it has no view tables and is read by key through a history request. |

**Axes on measures datasets.** A measures dataset may declare `axes = [...]`
naming columns of `role = "axis"`, each utf8 or f64 with a required
`axis = "bucket"` or `"scenario"` and, on a utf8 axis, an optional
`order = "tenor"` (`1M 3M 1Y 18M 2Y` by length, case ignored, labels that do
not parse last; the default is lexical). The axes extend the storage key of
every grain that holds a measure, in `axes` order after the grain key; a
grain holding only attributes keeps the plain key, so an attribute is stored
once rather than once per bucket. An axis groups and scopes like a
dimension. A bucket axis partitions a total (tenor buckets of vega sum to
total vega), so sums along it are attributable; a grouping that omits a
scenario axis is non-attributable at that level and shows NULL, because a
sum across alternative states (spot shifts) is not a number that belongs to
any row. Refusals drop the dataset, because every table would otherwise lack
or misfile an axis: an axis without a kind, a kind or order outside the
vocabulary, a `date` (or any non-utf8, non-f64) axis, `order` on a numeric
axis, `axes` and axis roles that do not mirror (an `axes` entry naming an
undeclared or non-axis column, or an axis column missing from `axes`), an
axis named like a grain key column, and axes with no measure to index. See
[`schema/axis.rs`](../../crates/geode-core/src/schema/axis.rs) for the
vocabulary and tenor order.

Unknown family strings or malformed key/axes arrays drop a dataset before
family validation. A columnless measure dataset remains in the schema but
owns no grain table. A columnless document, reference or history dataset is omitted. Series validation can
retain a corrected dataset even while reporting errors.

Document payload axes, values, and attributes support f64, i64, utf8, and
date. Unsupported columns are dropped before validating the remaining
identity and shape. The document family reserves `book` for partition
metadata; a surviving declared column with that name removes the dataset.
Other storage metadata names produce warnings in the shared validator and
are not automatically removed. These readers therefore do not guarantee
that every accepted schema can be stored successfully.

`local = true` applies only to application-written document datasets. On
other families it reports an error and resets to false. Source declarations
cannot feed a local dataset.

Series and history `retention` limits superseded rows by receipt time;
`history` limits all rows by observation time. Both accept duration strings
and default to unbounded. Values too large for signed microseconds also become unbounded
with an error. Actual pruning and storage behavior are described in the
[data-path guide](data-path.md).

A dimension column may set `order = "lexical"` (the default) or
`order = "tenor"`; it orders that column's values wherever they are listed,
such as a history key column's enumerated values. Tenor order reads
`<n>D|W|M|Y` in any case as a length in days (D = 1, W = 7, M = 30,
Y = 365), so `18M` sorts between `1Y` and `2Y`; equal lengths and
unparsable values (`ON`, `spot`) sort lexically, the unparsable ones after
every tenor. Values stay distinct: `12M` (360 days) and `1Y` (365 days) are
two keys, not two spellings of one. An unknown order, or `order` on a column
that is not a dimension, drops the column with an error at
`datasets.<dataset>.columns.<column>.order`.

A column's `source_name` maps its schema name to a different source-file
name. Categorical defaults apply to utf8 dimensions. Explicit
`categorical = true` on non-string columns warns and is cleared; explicit
false is accepted silently.

Text-search checks are family-specific: documents allow identity dimensions
only; a reference dataset keeps `textual` on key columns and clears it
silently elsewhere; measures accept a declared measure/attribute grain or a dimension
carriable by any built-in grain. A refused textual flag is cleared without
discarding the column. The measure check does not require an actual carrying
table in that dataset. For example, a textual underlying dimension can pass
validation in a dataset whose only declared storage grain is position, then
fail query routing. The query compiler checks the dataset's available grains.

## Grain and derived dimensions

[`Grain`](../../crates/geode-core/src/schema/grain.rs) orders position,
instrument, underlying, and underlying-pair storage keys from coarse to
fine. Their key lists form a prefix chain, but their dimension meaning is
not identical. The pair table stores canonical least/greatest underlying
identifiers. Treating one of those identifiers as the ordinary underlying
dimension would exclude pairs where the desired underlying occupies the
other position. Pair dimension keys therefore stop at instrument.

A carried dimension is a payload property determined by its declared grain.
A grain carries it only when its dimension key contains the entire declared
grain key. No grain can carry an underlying-pair dimension under this rule,
so the schema reader drops that declaration. A carried dimension does not
create a grain table by itself; measures and attributes determine the
available storage grains.

[`DerivedDimensions`](../../crates/geode-core/src/dimensions.rs) reads
many-to-one maps such as book to desk. A source value assigned conflicting
labels warns and retains its first assignment. Missing `from` drops the
dimension, while missing or non-table `values` yields an empty map. A
non-array mapped value warns; non-string elements inside arrays are skipped.
The reader does not validate `from` against a dataset. `base_column` performs
one lookup, so chained derived dimensions are not recursively resolved.

A classification created at runtime is held to stricter rules at creation
than the reader applies to a hand-written file
([`classification::validate`](../../crates/geode-core/src/classification/validate.rs)).
Its name must be an identifier (`[A-Za-z_][A-Za-z0-9_]*`) so a scope
expression can name it bare. It may not be a scope-expression keyword
(`and`, `or`, `not`, `in`, `like`, `true`, `false`), `config_version`, a
dataset column, or an existing derived dimension, each compared without
regard to case. The expression parser matches keywords ignoring case, so
`NOT` still negates, and DuckDB resolves identifiers ignoring case, so `Book`
beside the column `book` would be ambiguous in the compiled SQL. Its source
must be a column that is groupable in some dataset and `utf8` wherever it is
declared, and never itself a derived dimension: classifications do not chain,
since `base_column` resolves only one level. Rename and delete do not rewrite
the groupings, views, saved scopes and named expressions that name a
classification; `validate::references` counts them so the confirmation can
say how many will break.

The Classifications tile is `dimensions`' runtime writer, and it writes each
entry whole: the classification's `from` and every label's source list, as
`classification::to_toml` renders it (labels and sources sorted), through
the config door. A hand-written entry's comments and key order inside that
object do not survive an edit from the tile; the rest of the file does.

## Views and presentation

[`ViewSpec`](../../crates/geode-core/src/view.rs) reads dataset, joins,
selected columns, grouping, sort, and per-column display properties. Parsing
and schema validation are separate. `validate` checks the primary dataset, join
datasets, each join's keys against both the joined dataset's grains and the
view's own grouping, selected-column references, a `measure` column's role in the
primary dataset, a `dimension` column's reachability through the grouping, a
join, or a declared grain carrying it alongside the whole grouping (the
unanimity rule; `ViewSpec::ungrouped_dimensions` lists those columns for both
validation and the compiler), and grouping references. Derived dimensions
must resolve to a `utf8` source column in the primary dataset; a non-text
source is refused (see [data path](data-path.md)). It does not validate
derived SQL or sort keys. The compiler emits them into SQL; DuckDB binding
and execution can reject them.

The data service refuses a view by name when validation reports an error.
A column's `kind` defaults to `measure`; joins and columns default to
`required = true`. Setting `required = false` downgrades unusable joins,
measure-role mismatches, and unreachable dimension columns to warnings and
allows those declarations to be omitted. Unknown columns and invalid source
columns for derived dimensions remain errors. The flag does not affect
validation of derived SQL during compilation.

A top-level string `default = "name"` marks that view when present. A table
named `default` is an ordinary view. Without a string default, parsed views
are sorted by name for deterministic fallback. An explicit but unknown
name warns, leaves all views unmarked, and preserves declaration order.

Presentation has two separate precedence rules:

1. Configuration layers replace whole named view or presentation objects.
2. After that merge, display properties combine individually: kind default,
   view definition, dataset presentation, then view presentation.

View definitions nest numeric format keys under `format`; presentation
column tables put them beside `label` and `width`. The color key is `color`.
The old spelling `colour` is still read when `color` is absent, with a warning
at the old key; beside `color` it is ignored with a warning. The dialogs write
`color`, and saving a view through the Views dialog rewrites an old `colour`
key in its column formats the same way the reader resolves it. Dataset presentation
applies only to selected columns owned by that dataset. Ownership searches
the primary dataset first, then joins in declaration order. Unknown datasets
or columns warn and are skipped.

View presentation additionally controls order and visibility. Columns omitted
from a personal order remain afterward in their prior order, so adding a
column to the desk view still reaches users with personal ordering. Unknown
or repeated order entries warn. Hidden columns remain in the selected column
set and query result.

The supported top-level `hidden` array and `width` map are folded into
per-column properties. Explicit values in column tables win conflicts with
those spellings. Diagnostics retain the spelling that created an entry, so
an unknown column warning points to a key actually present in the file.

## Market-data panels

The `panels` reader has two halves, both pure. [`read_panels`](../../crates/geode-core/src/panel/read.rs)
judges each panel alone: its name, required keys, value shapes, formats,
choices, row identity and label, label uniqueness, columns named once, and
kind-action ids against a `KindActionRegistry`, which resolves each id to its
registered title and built state. [`load_panels`](../../crates/geode-core/src/panel/check.rs)
runs it, then checks each surviving panel against the schema and the
registered document kinds: the dataset is a declared document dataset, the
kind fits it (`check_kind_against`), the axes, column roles and types agree,
a pivot leaves one grid value, slice labels cannot collide with axis labels,
and every column the kind writes is named. The full list is in
[configuration](configuration.md#market-data-panels).

Unlike the view readers, nothing here only warns. The first problem refuses
the whole panel with one Error at `panels.<name>` or
`panels.<name>.<field>` (for example `panels.cvi.dataset` or
`panels.dividend.columns.values.3.format.precision`), attributed to the
layer that supplied the panel; its neighbours still load. An unknown key is
refused because a misspelt `format` would otherwise paint a plausible wrong
grid. Accepted panels keep document order, and `config_version` is skipped.
`geode-app` additionally refuses a panel named after another module's kind
and builds one tile kind per accepted panel.

## Scenario shapes

The `scenarios` reader has two halves, both pure, shaped like the panels
reader. [`read_scenarios`](../../crates/geode-core/src/scenario/read.rs)
judges each shape alone: its name, required keys, exactly one `columns` form,
the `value`, `diff` and `paint` rules, formats, and labels named once.
[`load_scenarios`](../../crates/geode-core/src/scenario/check.rs) runs it,
then checks each surviving shape against the schema — the dataset is a
measures dataset with `axes`, every dataset axis is `rows` or in `columns`,
every named measure is one, the diff base fits the rows axis's type — and
fills in what only the schema knows: the dataset's axes (kind, order, type,
in dataset order) and each measure's `_usd` twin. The pivot reads a shape
only after this half has run; one straight out of `read_scenarios` has no
axes and refuses to pivot. The full refusal list is in
[configuration](configuration.md#scenario-shapes).

Nothing here only warns. The first problem refuses the whole shape with one
Error at `scenarios.<name>` or `scenarios.<name>.<field>` (for example
`scenarios.rho.columns.axis.1` or `scenarios.spot_ladder.diff.base`),
attributed to the layer that supplied it; its neighbours still load.
Accepted shapes keep document order, and `config_version` is skipped.

## Grouping slots and saved scopes

[`GroupingSlots`](../../crates/geode-core/src/groupings.rs) reads slots 1–9.
Each array replaces that slot across layers. Invalid slot numbers, non-array
values, and empty groupings warn and are ignored; non-string array elements
are skipped. A name absent from every dataset and derived dimension reports
an error and drops its slot. This is an existence check: it does not prove
that a column is groupable or that one dataset carries the entire sequence.
Editors use the narrower `DatasetSpec::groupable_columns` vocabulary.

[`Scope`](../../crates/geode-core/src/scope/mod.rs) contains dimension
selections, named-expression references (`named`), text, an expression, and
an `impossible` flag. Within a query, these predicates combine with AND.
Across scope layers, selections on the same dimension intersect, named
references append without repeating one already present, expressions combine
with AND, and inner text replaces outer text when supplied. Empty selections
represent no constraint unless `impossible` is set. Disjoint intersections
set that flag and retain the dimension name so the UI can explain the
contradiction.

`Scope::resolve` folds every name in `named` into `expression` (list order,
ANDed together, then ANDed with whatever expression was already there)
against a [`geode_core::named::NamedExpressions`](../../crates/geode-core/src/named.rs)
read from `expressions.toml`, and returns a scope whose `named` is empty. The
first name that document cannot supply — absent, or kept `Invalid` because it
failed to parse — is an error naming that reference rather than a skip;
skipping it would silently widen the scope. This is a caller-driven step, not
part of `and_then`: a scope can carry unresolved names for as long as it is
pure shell state, and every place a scope reaches a query resolves it first
(see [the shared frame](shell.md#the-shared-frame)). `NamedExpressions` itself
is built at startup and rebuilt whenever `expressions`, `datasets`, or
`dimensions` changes on reload; an entry naming a column the schema lacks
stays `Valid` and only warns, the same as any other scope expression's
unknown-column check.

Composition currently handles empty selections asymmetrically: empty inner
entries are skipped, but outer entries are copied before intersection. An
outer `book = []` combined with inner `book = ["A"]` therefore sets
`impossible` instead of adopting the inner selection. Unmatched empty outer
entries are dropped unless already recorded as contradictions. Readers can
retain empty selections, so this limitation is not restricted to manually
constructed scope values.

Scope validation checks referenced names against one dataset, resolving
derived labels through their source columns. Derived-label expressions
allow equality, inequality, and membership; ordering and `like` are rejected.
This validation does not check literal types or all query-routing rules.
`applicable_to` removes unavailable dimension selections and returns their
names for provenance. It preserves text, expressions, and `impossible`; it
does not make an arbitrary expression valid for another dataset.

[`saved_scopes_from_doc`](../../crates/geode-core/src/scopes.rs) validates
against each dataset separately and chooses the smallest diagnostic set.
With a nonempty schema, at least one dataset must validate the complete
scope; columns spread across datasets do not form a union. With an empty
schema, this step produces no errors. Rejected scopes return warnings and
are omitted. This does not provide strict field validation: a non-table
`dimensions` field is ignored, non-array dimension values become empty
selections, and arrays retain only string elements. Non-string `text` and
`expression` fields are ignored. These forms produce no shape diagnostic;
for example, `dimensions.book = 7` yields an empty selection instead of a
book constraint. Such malformed fields can therefore weaken the loaded
scope. Empty selections also have the composition limitation above.

The reader does not reject reserved action names; creation paths reserve
`save_current`, and action registration separately skips occupied action IDs.
`named` is read as an ordered, deduplicated array of strings; a non-array
value warns and is ignored, but, like a malformed dimension field, does not
drop the scope. Names are not checked against `expressions.toml` here —
`Scope::resolve` is where a missing or invalid one fails — so a scope
naming an expression not yet defined anywhere is kept rather than dropped.

Saved-scope serialization writes selections, named-expression references,
text, and expression. It omits empty selections and an empty `named` list,
and does not persist `impossible`, so it is not a lossless format for every
composed scope. The
[expression parser](../../crates/geode-core/src/scope/expr.rs) accepts
comparisons, membership, boolean operators, and parentheses. Parsing is
schema-free; statements, comments, functions, and subqueries are outside its
grammar. Quoted strings escape internal quotes by doubling them.

## Watchlists

[`watchlist`](../../crates/geode-core/src/watchlist/mod.rs) reads
`watchlists.toml`: named lists of underlyings, each a top-level table that
replaces whole across layers.

```toml
[europe]
include = ["SMI"]            # names by hand
exclude = ["UKX"]            # names never live, whatever produced them
[[europe.rules]]             # zero or more
dataset = "risk_snapshot"
expression = "region = 'Europe'"   # or scope = "<saved scope>"; at most one
```

A list is dropped, with an error, only when its object is not a table, when
its name is refused, or when a name is in both `include` and `exclude`. A
name must be an identifier (letters, digits and `_`, not starting with a
digit), not a scope keyword, not `config_version`, and not a clash ignoring
case with an earlier table in the document; a clash is judged against every
earlier table, kept or dropped, so fixing an earlier list cannot make a
later one start clashing. `validate_name` applies the same rules to a new,
cloned or renamed list. Every other problem warns and keeps the list: name
arrays are trimmed, blanks dropped, repeats kept once in first order; a
non-array warns and reads as empty, and a non-string entry warns and is
dropped. Rules are kept whatever their shape (a non-table rule reads as an
empty one, a missing or non-string `dataset` as none) so the fold can report
them by index.

`fold::fold_rules` checks each rule against the schema the data service
started with, the saved scopes and the named expressions, and answers the
good rules as `ResolvedRule`s in definition order and the rest as
`RuleError`s by index. A rule error never drops the list: no dataset named;
a dataset the schema lacks (one added since launch needs a restart); a
computed, local, reference or series dataset, or one without an
`underlying_ref` column (`eligible_datasets` lists what a rule may name:
stored measures or document datasets, not computed, not local, carrying the
column); both a scope and an expression; a saved scope that is not defined
or selects nothing; an expression that does not parse; a named expression
the scope cannot resolve; or a scope the dataset cannot honour under the
same validation a saved scope gets.

Members are the set algebra over the names the good rules produce:
rule results ∪ `include` − `exclude`, sorted by name, each `Member` with its
`Origin` (`Manual`, `Rules(indices)`, `Both(indices)`, or
`Excluded { rules, manual }`). An excluded name is never a live member,
whatever produced it, but it stays in the list as `Excluded` so a consumer
can show and restore it; an exclusion no rule produces and `include` does
not hold is still listed, as the exclusion exists and may be worth dropping.
`WatchlistSnapshot::members_of` reads the live members and `all_names` every
name, excluded ones included. Resolution itself runs in the data layer and
is live only, never at the frame's as-of, with each rule's distinct in its
own read transaction, so a rule the data layer cannot compile or run joins
the fold's errors by index while the others still answer; see
[queries and time travel](data-path.md#queries-and-time-travel) and
[the watchlist cache](shell.md#state-ownership).

The edit verbs (`edit::add`, `remove`, `set_rules`) return the whole next
object and an `UndoEntry` holding only what changed. `add` includes a new
name by hand, restores an excluded one (an orphan exclusion, with no rule
and not manual, is included by hand instead, or clearing it would leave the
name nowhere), and refuses a name already a live member, naming the rule
that supplies it; `remove` drops a manual name from `include`, excludes a
rule-derived one, does both for a name that is both, and restores an
excluded one; `set_rules` replaces the rules whole as one change. `undo`
replays an entry backwards over the current object and skips a row whose
manual state (or rules set) is no longer the entry's `after`: it was changed
elsewhere, and restoring a stored snapshot would silently revert that edit.
`to_toml` writes `include`, then `exclude` and `rules` when nonempty, each
rule `dataset` then its `scope` or `expression`, and reads back equal.

## Vol views

[`volviews`](../../crates/geode-core/src/volviews.rs) reads
`vol_views.toml`, the vol watchlist's tabs: one top-level table per view
in document order, each with a `columns` array, and a top-level string
`default`. `from_doc` parses without the schema and keeps every column
whose text reads, answering a `VolViewsDoc` of `VolViewSpec`s whose
measures are `MeasureText` (a built-in `Measure`, or a `Table` name with
its key words). `resolve` turns each open-form text into a board
`Measure::Table` against a `TableShape` lookup (a dataset's key length and
value-column count, derived from the schema by the caller) and answers the
`VolViews` the tile shows, with `default` as an index.

A column is one of `measure = "<text>"`, `spark = "<text>"` (with an
optional `span`, a non-zero duration, `30d` by default) or `kind =
"vega"`. A measure column may also carry `label` and `group` (the sub-head
and the group head; blank reads as absent) and two glyph keys: `level`,
one of `plain`, `heat` or `tint`, how the level cell paints, and
`companion`, one of `chip`, `bar` or `slider`, how the companion cell
paints. The keys are exact lower-case words: the file is written by the
dialog or by hand, and a near miss is reported, not guessed. Absent means
the view's default (a plain level and a chip in a pair view; a tinted
level in a single view), which the tile resolves, so the reader carries
`Option`s and a writer never spells a default. The glyphs belong to a
measure column: on a spark or vega column either key drops that column.

A measure text is one of:

| Text | Measure |
|---|---|
| `point <tenor> <delta>` | `Point` (call delta as the surface stores it: `0.25` is the 25Δ call, `0.75` the 25Δ put) |
| `atfm <tenor>` | `Point { delta: 0.5 }`; the half-delta point reads back as `atfm` |
| `forward <tenor>` | `Forward` |
| `skew <tenor>` | `Skew` (vol(0.75) − vol(0.25)) |
| `wing skew <tenor>` | `WingSkew` (vol(0.90) − vol(0.10)) |
| `slope <long>-<short>` | `Slope` |
| `realized <window>` | `Realized` |
| `iv-rv <tenor>/<window>` | `IvRv` |
| `varswap <tenor>` | `Varswap` |
| `vs spread <tenor>` | `VsSpread` |
| `<dataset> [<k1>] [<k2>]` | `Table` over a `history` dataset |

Keywords read case-insensitively. Tenor, window and delta words are the
data's key spellings, normalized as the surface reader normalizes them
(`0.50` reads as the `0.5` point), and a point delta must be a finite
number strictly between 0 and 1. The open form names a `history` dataset
by its schema name and qualifies when the dataset's key is `underlying_ref`
followed by zero, one or two more columns and it has exactly one value
column; the words after the name are the remaining key values in key
order (`term_slope`, `ssr 3M`, `smile_curv 3M 0.25`), whatever the key
columns are called. A built-in keyword is never read as a dataset name, so
a dataset called `skew` is unreachable by the open form and the resolver
says so.

Failure semantics, each a warning at its key (`vol_views.<view>`,
`vol_views.<view>.columns.<n>`, `vol_views.default`), never an error, so
a bad tab cannot cost a desk its others: a column with none or more than
one of `measure`, `spark` and `kind`, a measure or spark text that does
not parse, a `span` that is not a non-zero duration, a `kind` other than
`vega`, a `level` that is not `plain`, `heat` or `tint` (`level = "loud"
is not plain, heat or tint`), a `companion` that is not `chip`, `bar` or
`slider`, or either glyph key on a spark or vega column (`level applies
to a measure column`) drops that column alone (`vol_views: <view> column
<n>: <why>; dropped`); `cells` other than `pair` or `single` warns and
reads as `pair`; a view that is not a table is skipped (`vol_views:
'<view>' must be a table; skipped`). Two rules keep the document the
dialog's: a view with no `columns` array, `columns = []`, or every column
dropped is kept, empty, with `vol_views: '<view>' has no columns` — a
table with only `cells` is a legal empty tab the dialog fills later, and
skipping it would drop the tab the user is about to fill; and a second
view whose name matches an earlier one case-insensitively is skipped with
`vol_views: '<view>' is defined twice; the second is skipped`, since the
tile's `view <name>` lookup and the dialog's name prompt could otherwise
hold two tabs a reader cannot tell apart (one TOML layer cannot repeat a
key, and the merge collapses a cross-layer repeat, so the case collision
is the one duplicate that can arrive). At `resolve`, an open-form column
whose dataset the lookup lacks, has more than one value column, or takes
another number of key words than the text supplies drops alone, a view
left with no column resolves to an empty view silently, and a `default`
naming no resolved view warns and the first view opens (`default` is `0`,
also with no view at all). Resolution runs
in the app (`volwatch_settings_from_config`) against the schema the data
service started with, as watchlist rules are, with a lookup that admits
only a `history` dataset whose first key column is `underlying_ref`; a
document that resolves to no view warns `vol_views: no valid view; the
tile shows the builtin tabs` and the builtin document stands in. The
resolved `VolViews` travels inside `VolwatchSettings` and is pushed whole
on every reload, so the tile never re-reads the document itself.

The same module holds the pickers' model and the writer the Vol views
dialog uses. `Family` is the grammar's forms plus the two non-measure
columns and an open-form dataset (`Table(name)`), each saying which parts
it takes (`takes_tenor`, `takes_delta`, `takes_span`, `takes_window`,
`takes_slope`, `takes_glyphs`) and its catalogue label (`ATFM`, `25Δ
skew`, `Surface point`, a dataset's upper-cased name). `ColumnChoice` is
a column as the pickers hold it: the family, its parts, label, group and
the two glyphs as `Option`s; `ColumnChoice::new` opens a family at its
firsts (the first tenor, the first delta, a 30-day span, the 20-day
window, `1Y-1M` for a slope), `from_spec` reads a parsed column back into
a choice or answers `Raw { text }` for a column the pickers cannot hold (a
dataset with two key words, a spark over anything but an ATFM point),
`to_spec` writes it, `default_label` and `default_group` are what the
tile shows when the column names none, and `head` is the list row's text
(`ATFM 3M · heat · bar`, a named glyph appended, `plain` included). The
picker lists are `ALL_TENORS`, `DELTAS`, `WINDOWS` and `SPANS`; the reader
accepts any tenor, window or duration, so a hand-written value the lists
lack is kept as the row's own option rather than read as the first one.

`to_table(view, source)` renders one view over its source table when it
has one, so a key inside the table the writer does not model and the
table's comments survive: `cells` is written only for a single view, and
`columns` is rebuilt from the specs, each column carrying only the keys it
sets — a default label, group or glyph is never written, a `span` only
when it is not 30 days, and a measure as `measure_text` spells it, so a
hand-written `point 3M 0.5` reads back as `atfm 3M` on the first save and
the builtin tabs round-trip parsed-equal (not byte-equal). A key inside a
`[[columns]]` table that the reader does not know, and a comment on one,
do not survive the rewrite. `span_text` writes a span at the coarsest
unit of the duration grammar that spells it exactly (`7d`, `12h`, `90s`,
`500ms`, `1y` for 365 days), never a rounded `0d` the reader would
refuse. `to_document(doc, source)` writes the whole file: `config_version`,
`default` when set, the views in the doc's order, each over its source
table, then any top-level value the source held that the writer does not
model; a source table the doc no longer lists is a deleted view and is not
copied, and every table is renumbered so the doc's order reaches the
file. `reorder(document, order)` moves the named view tables to the front
in `order`, tables not named keeping their relative order after them and
`config_version` and `default` first; tables are removed and re-inserted,
so each keeps its comments.

## Colors and numeric formatting

[`NamedColours`](../../crates/geode-core/src/colour/mod.rs) reads a hue or a
semantic theme token per name. Supplying both or neither, an invalid name,
an invalid hue, or an unknown token drops the definition with an error.
`none` and `sign` are reserved for column color modes, and a name starting
with `#` is reserved because `#rrggbb` spells an absolute color where a name
is also accepted (a timeseries slot's color). Hue values range
from 0 through 360, with 360 normalized to zero. Invalid string tones warn
and fall back to normal; a tone beside a token warns and is ignored.
Malformed `tint_sign` warns and becomes false.

Hues interpolate between theme anchors in OKLCH, then seek a 3:1 contrast
ratio against the theme background by moving lightness toward the theme
foreground. This is a target, not a guarantee for arbitrary themes: if the
available lightness path never reaches that contrast, its endpoint is
returned. Untinted semantic tokens retain their exact theme color. Sign
tinting applies contrast adjustment to all three variants, including zero.

[`ValueColors::from_doc`](../../crates/geode-core/src/colour/values.rs) reads
`value_colors.toml`: one table per dimension, each mapping a value's text to a
`colors.toml` name or an inline `{ hue }` / `{ token }` table. Values match
exactly, so `spx` is not `SPX`. An inline table is read by
`Definition::from_table`, the same reader `colors.toml` uses, so the two
cannot drift; `tint_sign` is refused there because a value has no sign, and
each refusal is an error at the entry's path (plus `.hue` or `.token` for a
field) that drops the entry. An inline entry is stored under the internal key
`inline {dimension}.{value}`, which `check_object_name` refuses, so no color
name can ever equal it. `NamedColours::get` resolves inline keys, so every
paint path does, while `NamedColours::names()` and every listing omit them.
Five shapes are refused with an error that drops the entry: a dimension that
is not a table, an entry that is neither a string nor a table, `sign` (a
column color mode, not a color), a name starting with `#` (an absolute
color), and an empty value. An
entry of `none` reads as unmapped without a diagnostic; it is how a higher
layer clears a lower layer's color. A dimension whose entries are all cleared
or refused is absent. The reader does not consult the schema or the color
definitions.

[`check_value_colors`](../../crates/geode-core/src/colour/values.rs) then
prunes what cannot paint. A dimension is text, per `dimension_kind`, when some
dataset declares it as a utf8 `dimension` or `key` column, or when it is a
derived dimension (its values are labels); `text_dimensions` lists exactly
those names. Three warnings each remove their entry: a dimension no dataset or
derived dimension declares, a declared dimension that is never text (a numeric
`strike` is not colored by its printed number), and a value naming a color
`colors.toml` does not define, which paints without a color. Inline entries
take the two dimension checks and skip the unknown-name check, since they
name no `colors.toml` color; a pruned dimension drops its inline definitions
with it. A value is inline only when its color key is its own
`inline {dimension}.{value}`: a string entry spelled like another value's
inline key borrows nothing, takes the unknown-name warning, and is pruned.
What survives is
exactly what a tile may look up, so paint performs no second validity check.
`NamedColours::from_config` returns the color definitions together with the
checked mapping (`NamedColours::values`), so one `Arc<NamedColours>` never
pairs a mapping from one reload with definitions from another; its
diagnostics are the colors reader's, the value reader's, then the check's.

[`format_number`](../../crates/geode-core/src/format.rs) scales, rounds, then
applies grouping and negative notation. Its returned sign follows the
rounded result, so a small negative rounded to zero receives zero styling.
NaN displays as `NaN` with zero sign; infinities display as `∞` or `-∞`.
