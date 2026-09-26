# Geode — Silent Wrong Data Design

The second work unit from the 2026-09-25 codebase review
(`docs/superpowers/reviews/2026-09-25/`), which the review's decomposition calls
group C. Six defects across three crates, each of which puts a wrong answer on
screen without saying so.

Every finding below was re-verified against `main` at f7d92828 before this
document was written, because the review is a dated snapshot and main had moved
six commits. All six are still present.

Amends `docs/current/configuration.md` (view validation becomes strict),
`docs/current/data-path.md` (`Freshness::generation` gains a meaning), and the
blotter, market-data and geode-data crate READMEs.

## 1. The principle

One sentence governs all six: **identity over position, and refuse rather than
guess.**

Each defect keys something by where it happens to sit, or substitutes a
plausible value for one it could not determine. The blotter keys a sort by a
column's position in a list that reorders. Market-data keys a generation by
source time alone, so a republish at the same time is indistinguishable from no
republish. The compiler sums a column whose role it could not classify and drops
a join it could not resolve. The market-data cell path narrows an integer
through a double it did not need to pass through.

None of them errors. Each produces a number, an order, or a blank that a trader
reads as fact. `CLAUDE.md` ranks that above an explicit error, which is why
these six are one unit and why every fix below moves toward refusing or
re-identifying rather than toward a better guess.

## 2. Three branches

The six defects merge independently and are three branches, not one. Each
delivers working software on its own and can be stopped after.

| Branch | Crates | Defects |
|---|---|---|
| Sort identity | `geode-blotter` | `SortSpec.column` and `Cursor.col` are positions |
| Generation identity | `geode-data`, `geode-core`, `geode-marketdata` | same-source-time republish; I64 cell round-trip |
| View strictness | `geode-core`, `geode-data` | unresolvable joins; non-measure summed |

Each gets its own implementation plan. This document is the shared argument.

## 3. Branch one: sort identity

### 3.1 What is wrong

`SortSpec.column` is a `usize` documented as "Index into `ColumnPlan::columns`"
(`crates/geode-blotter/src/core/flatten.rs:95-99`). Two paths break it.

`ColumnPlan::move_column` does a remove-and-insert on that vector, shifting
every index between the two positions. The delegate's `move_column` hook calls
it and invalidates the format cache; it never touches `self.sort`. So after a
drag, the recorded sort names a position now occupied by a different column. The
rows do not move until the next reflatten, at which point the siblings reorder
by a column the trader never chose, and the header paints its arrow on whatever
now sits at that index.

A plan rebuild is worse because it acts in the same frame:

```rust
self.sort = self
    .sort
    .filter(|s| s.column < self.plan.as_ref().unwrap().columns.len());
```

That is a bounds check, not an identity check. Hiding a column or regrouping a
dimension into the tree column shortens `columns` and shifts everything after
the removal down one, so a surviving in-range sort names a different column and
`reflatten_keeping` immediately reorders by it.

`Cursor.col` (`crates/geode-blotter/src/core/cursor.rs:11-14`) has the same bug
in a milder form: after a move the cursor rests on a position rather than on the
column the trader was looking at, so a subsequent `s` cycles the sort of
something else.

### 3.2 What it becomes

`SortSpec.column` becomes the column's name. `:sort <col>` already resolves a
name against the plan, so the name is the identity the vocabulary already uses,
and no new concept is introduced.

- `move_column` needs no remap at all. A name is stable under reorder, which is
  the point of the change: the bug becomes unrepresentable rather than handled.
- A plan rebuild re-resolves the name against the fresh plan.
- When the column is gone, the sort drops and the tile's notice
  (`error: Option<String>`, `tile.rs:194`) names it. The rows reorder either way
  because dropping a sort restores default order, so the only question is
  whether the trader is told why, and they are.
- `Cursor.col` follows the same identity, for the same reason.

### 3.3 Failure semantics

A sort whose column disappears is not an error. It is a state change the trader
caused, and the notice explains it. A sort naming a column that never existed
cannot arise, because `:sort` validates at parse and a session restore resolves
against the plan or drops.

## 4. Branch two: generation identity

### 4.1 What is wrong

A generation's identity is its source time alone. `Draft.base` is
`Option<String>` holding an RFC 3339 source time
(`crates/geode-marketdata/src/core/draft.rs:158-162`), and `on_delivered`
returns `false` when the delivered `as_of` equals it, leaving the draft
`Editing`.

`apply_snapshot` nonetheless rebuilds from the new snapshot and installs it.
`Draft::edits` is keyed by document row and model column, and under an axis
layout the model's column order is the document's own node order, deliberately
unsorted. So a republish that keeps its source time and reorders or adds a node
re-points every edit onto a different node, paints it there, and `:upload` will
assemble and send it. The draft's label side map would detect this but is
consulted only by `rebase`.

The draft's own comment names the fix: a generation id in the provenance.

Separately, `parse_cell` is `fn parse_cell(text: &str, ty: ColumnType) ->
Result<f64, String>` and its I64 arm does `.parse::<i64>().map(|v| v as f64)`.
Eight lines below, `parse_attr` does `.parse::<i64>().map(Value::I64)` under a
comment stating that a round trip through a double "loses every integer above
2^53, silently". The same typed value is therefore exact in an attribute and
truncated in a cell. `bumped` takes and returns through `f64` for the same
reason.

### 4.2 What it becomes

The carrier already exists and needs no new plumbing. Market-data reads
`snapshot.provenance().datasets.first().as_of`, and `Freshness` already has
`generation: i64` beside `as_of` (`crates/geode-core/src/snapshot.rs:28-33`).

That field is currently written in two live places, both as
`catalog.latest_gen_id()`, which is `max(gen_id)` over the whole database rather
than the dataset's own, and the as-of arms write literal `0`. No production code
reads it. So the field is present, wrong, and unread, and giving it a meaning is
the same work as fixing it. This closes the review's separate finding that live
provenance reports a database-global counter.

- `Freshness::generation` becomes the generation of the data actually read, per
  dataset. The as-of arms report the resolved generation rather than zero.
- `Draft`'s base becomes the pair of source time and generation. `on_delivered`
  compares the pair, so a republish keeping its source time now differs.
- A differing generation puts the draft `Behind` with a notice naming it, which
  routes into the `rebase` and `revert` machinery the trader already knows. No
  third behaviour is invented.
- `parse_cell` gains a `Value`-returning sibling so the I64 arm never becomes an
  `f64`, and `bumped` does integer arithmetic for an I64 column.

### 4.3 Failure semantics

A republish is not an error and must not discard work. `Behind` is the existing
state meaning the document moved under an open draft, and it already offers
rebase and revert. The change is only that a same-time republish now reaches it
instead of being invisible.

An I64 value too large to represent exactly is refused at parse with the message
`parse_attr` already uses, rather than silently truncated.

## 5. Branch three: view strictness

### 5.1 What is wrong

`ViewSpec::validate(&self, schema: &SchemaSpec, dims: &DerivedDimensions) ->
Vec<Diagnostic>` (`crates/geode-core/src/view.rs:324`) documents itself as not
validating join keys or column roles. The compiler therefore meets three
situations it cannot honour and guesses at all three
(`crates/geode-data/src/query/compile.rs:686-701`):

```rust
let Some(joined_ds) = schema.dataset(&join.dataset) else { continue; };
if !join.on.iter().all(|k| materialized.contains(k)) { continue; }
let Some(joined_grain) = joined_ds.grains().into_iter()
    .find(|g| carries_all(joined_ds, *g, &join.on, dims)) else { continue; };
```

The middle `continue` is correct and well reasoned: the key is not on the
bounded spine, so the columns would be NULL anyway. The other two are
configuration errors. The result compiles, runs, and returns the joined
dataset's columns as absent rather than NULL, so `Snapshot::column_index`
answers `None` and the blotter paints an empty column with no diagnostic
forever.

A fourth site: a column declared a measure that is not one is dropped from the
statement by a `filter_map`, and one that carries a grain without being a
measure reaches `_ => Aggregate::Sum` (`compile.rs:433`) and is summed. Since a
grain-bearing attribute repeats across its grain's rows, summing it produces a
plausible wrong total. The join path already gets this right, using `any_value`
because attributes should agree.

This matters more than it looks because `kind` defaults to `"measure"` when
absent in the view reader. A column written with just a name takes the measure
path, so the most probable authoring mistake is the silent one.

### 5.2 What it becomes

Validation moves to load time, where every other configuration error surfaces
and where the dialogs already render diagnostics addressed to
`views.<name>.<field>`.

`ViewSpec::validate` gains three checks:

1. the join's dataset exists;
2. the join's keys are carried by some grain of the joined dataset;
3. each column's declared kind matches its actual role in the schema.

Anything unhonourable **refuses the view** with an error diagnostic, unless that
join or column carries `required = false`, in which case it is dropped with an
informational diagnostic naming what went.

`JoinSpec` and the view column forms gain `required: bool`, defaulting to
`true`. This reuses vocabulary the codebase already has: `ColumnSpec::required`
defaults to `true`, and the ingest path distinguishes `missing_required`, which
raises a health warning, from `missing_optional`, which is tolerated. The
declaration sits where it applies rather than in a global policy, and the author
says which they meant instead of a heuristic guessing per kind.

The compiler's two configuration-error `continue`s become `compile_error`, so
validation is the gate and the compiler cannot silently drop anything. The
middle `continue` stays, with its reasoning intact. The `Sum` fallback becomes
unreachable and is restructured away rather than left as a trap.

### 5.3 Failure semantics, and the accepted risk

This makes a load-time contract stricter, so **a view that works today by being
silently wrong will refuse tomorrow.** Given the `kind` default, any desk
`views.toml` column written with just a name that is not a measure becomes a
refused view on upgrade, and a refused view is a blotter that does not open.

The ruling of 2026-09-26 is to keep it strict and revise if practice argues
otherwise. The reasoning is that the alternative is the defect: a view that
opens while lying about what it shows. A refused view with a diagnostic naming
the column is a five-second fix; a blank column nobody notices is not.

Two obligations follow. The shipped demo configuration and every builtin view
must validate clean, proved by a test. And the diagnostic must name the column
or join and the reason, because it is the entire remedy the trader gets.

## 6. Documentation

- `docs/current/configuration.md`: view validation now refuses an unhonourable
  join or column, and `required = false` is the opt-out.
- `docs/current/data-path.md`: `Freshness::generation` names the generation of
  the data read, per dataset.
- The three crate READMEs, for the invariants each branch changes.

## 7. Verification

All six are silent-wrong-data contracts, which is what the mutation harness
exists for, so each branch carries targeted entries for the contracts it
changes. The harness's filter linter now enforces that every entry names a real
test, so an entry added here that names nothing fails the gate.

Tests go at the lowest layer that proves the behaviour:

- Sort identity is pure: drag a column and assert both the painted arrow and the
  resulting row order; hide a column and assert the sort dropped and the notice
  names it; regroup a dimension into the tree column and assert the same.
- Generation identity is pure in the draft: a delivery at the same source time
  with a different generation puts the draft `Behind`. The provenance change
  needs a store-level test that the generation reported is the dataset's own and
  not the database maximum.
- I64 exactness is a parse test at a value above 2^53.
- View strictness is pure in `geode-core`: each of the three checks refuses, and
  `required = false` downgrades it to a dropped piece with a diagnostic. Plus
  the demo-config test above.

Gates before each merge: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, and
`zsh scripts/mutation-check.sh --anchors-only`.

## 8. Out of scope

Named so they are not mistaken for oversights.

- The review's remaining groups D through I, and the anchor-uniqueness follow-up
  from the previous unit.
- The `geode-dates` crate and per-underlying trading calendars, ruled on
  2026-09-26 as its own work.
- Making the dropped-versus-refused decision a global policy setting. Considered
  and rejected: it would be the one configuration key describing what to do when
  the configuration is wrong, and a desk that set it once would silence the
  diagnostic class permanently.
- Caching the config dialogs' row sets, which is group F. The performance guide
  was corrected to stop claiming a cache that does not exist.
