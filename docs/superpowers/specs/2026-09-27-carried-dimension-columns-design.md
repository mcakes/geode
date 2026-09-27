# Ungrouped dimension columns: shown where unanimous

Status: approved by Matthew 2026-09-27 ("Show where unanimous").

## Problem

A measure-family view may declare a `kind = "dimension"` column that its
grouping does not contain — `strike` or `expiry` beside a
`lhu → underlying_ref → position_ref` tree. The compiler selects only grouping
columns, measures, joined columns and derived expressions, so such a column
has no source. View strictness (spec 2026-09-26 §5, ruling 2) therefore
refuses the view at load with "column 'strike' is declared a dimension, but it
is neither in the grouping nor carried by a join, so no row supplies it". The
Views dialog offers every dimension-role column, so a trader can write a view
through the dialog that refuses on save.

The obvious fix — `any_value(strike)` at the position row — is wrong. A
position holds several instruments (the instrument key is the position key
plus `instrument_ref`), and `strike`/`expiry` are carried per instrument. In
the demo data 8,385 of 12,500 positions in one book have more than one strike.
`any_value` would paint an arbitrary leg's strike: a plausible wrong value,
which the product treats as worse than an explicit gap.

## Rule

On every tree row, at every depth including the grand total, an ungrouped
dimension column shows:

- **its value**, when every stored row under that tree row (inside the scope,
  in the query's era) has the same non-NULL value;
- **a distinct "mixed" marker**, when the rows under it disagree;
- **blank (NULL)**, when no row under it has a value (no rows, or every value
  NULL). A mix of NULL and one value is **mixed**, not that value — showing the
  value would claim something the NULL rows do not.

The marker is not NULL and not a value. Blank and mixed must be
distinguishable on screen, in sorting (mixed sorts with NULL, after values, or
as a documented stable choice), and anywhere the value is read as text
(copy/selection readouts treat mixed as not-a-value).

The rule is exact at every depth, so it needs no depth or grain reasoning to
be correct: a single-instrument position shows its strike; an underlying whose
positions all share an expiry shows that expiry; anything else says "mixed".

## Where the value comes from

The column is computed from one declared grain table of the view's primary
dataset: the **coarsest declared grain that carries every materialized grouping
column and the column itself** (`DatasetSpec::carries`, through
`DerivedDimensions::base_column` for grouping columns as elsewhere). Carried
values repeat identically across finer rows, so the coarsest carrying table
gives the same answer on the fewest rows.

It is aggregated over the same grouping sets as the spine, with a depth marker,
and joined to the spine on the grouping keys (`is not distinct from`) plus
depth, exactly as a measure grain's aggregate is. Use
`min(col) = max(col) and count(col) = count(*)` style aggregates (cheap; no
`count(distinct)`), producing the value and a mixed flag. When a measure
aggregate CTE already exists for the chosen grain, fold the extra aggregates
into it rather than scanning the table twice; otherwise emit one CTE per grain
covering every such column at that grain. Scope and era apply as for a measure
at that grain (`compile_scope_cached`, `era.relation`).

A dimension column that is in the grouping, or that a join supplies, keeps
today's path unchanged.

## Validation

`ViewSpec::validate`'s dimension reachability check (view.rs, "is declared a
dimension, but it is neither in the grouping nor carried by a join") accepts a
column when **some declared grain of the primary dataset carries every column
of `view.grouping` and the column**. It is still judged against the whole
grouping, never a query's bounded depth (ruling 3). Otherwise it refuses as
today, with the message updated to say what would have supplied it (grouping,
a join, or a grain carrying it alongside the grouping). `required = false`
keeps its meaning. Validation and compiler must agree: a view that validates
must compile at every `max_depth`, and the per-query grouping override is
judged the same way (ruling 7).

Scope of the new path: columns whose primary-dataset role is
`ColumnRole::Dimension`. Attributes, keys, derived dimensions and document or
series families are unchanged.

## Metadata

The compiled column is not summable, has no measure grain semantics for
attribution (`Additive` at every depth — the unanimity rule is already exact),
and `ScopeSemantics` from the chosen grain's scope. The Views dialog needs no
change: every dimension it offers either passes the new check or is refused
with the updated message.

## Performance

Views that declare no ungrouped dimension compile to the same SQL as before
(assert this). With `strike` and `expiry` added to the demo `tree` view,
measure the one-million-row requery against the 50 ms target and record it in
the measurement log with conditions.

## Known limitations

- The unanimity is over the chosen grain table's rows. An instrument that has
  no row in that table (for example a cash instrument absent from the
  underlying table when the grouping forces the underlying grain) does not take
  part. The coarsest-carrying choice minimises this; document it.
- The marker is presentation. A downstream consumer reading the snapshot sees
  NULL plus the flag, not a value.
