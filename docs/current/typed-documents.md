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
| `measures` | Columns declare roles and fixed grains. Measures and grain-bearing attributes create storage grains. Document-only roles and uncarriable dimensions are dropped. Missing grain-key declarations and reserved metadata names warn without removing the dataset. |
| `document` | A nonempty ordered key identifies a document; nonempty ordered axes identify its rows. Key columns must be grainless utf8 dimensions. At least one value is required. Invalid identity or row shape removes the dataset. |
| `series` | Storage implies `source`, `series_id`, `ts`, `received_at`, and `value`. Declared columns are dropped and nonempty key/axes arrays cleared with errors. Invalid retention windows report errors and become unbounded. |
| `reference` | One keyed table replaced whole per snapshot. A nonempty `key` of grainless utf8 dimensions identifies a row; every other column is a grainless attribute of utf8, f64, i64, date, or bool. `axes`, timestamp columns, and the storage names `batch`, `book`, `source_file_id`, `gen_id`, and `source_time` are refused. Any failure removes the dataset, because a half-understood mapping table would place rows wrongly. Storage order is the key columns, then the rest in declared order. A reference dataset offers no grouping vocabulary and is neither a document-kind nor an egress target. |

Unknown family strings or malformed key/axes arrays drop a dataset before
family validation. A columnless measure dataset remains in the schema but
owns no grain table. A columnless document or reference dataset is omitted. Series validation can
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

Series `retention` limits superseded rows by receipt time; `history` limits
all rows by observation time. Both accept duration strings and default to
unbounded. Values too large for signed microseconds also become unbounded
with an error. Actual pruning and storage behavior are described in the
[data-path guide](data-path.md).

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
must resolve to a source column in the primary dataset. It does not validate
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
`colors.toml` name. Values match exactly, so `spx` is not `SPX`. Five shapes
are refused with an error that drops the entry: a dimension that is not a
table, an entry that is not a string, `sign` (a column color mode, not a
color), a name starting with `#` (an absolute color), and an empty value. An
entry of `none` reads as unmapped without a diagnostic; it is how a higher
layer clears a lower layer's color. A dimension whose entries are all cleared
or refused is absent. The reader does not consult the schema or the color
definitions.

[`format_number`](../../crates/geode-core/src/format.rs) scales, rounds, then
applies grouping and negative notation. Its returned sign follows the
rounded result, so a small negative rounded to zero receives zero styling.
NaN displays as `NaN` with zero sign; infinities display as `∞` or `-∞`.
