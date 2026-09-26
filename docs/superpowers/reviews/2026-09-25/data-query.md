# Review: `crates/geode-data/src/query/` and the query types it consumes

Read-only review. Scope: `query/{compile,scope_sql,as_of,distinct,document,pool,read,series,catalog,mod}.rs`
plus the geode-core types they consume (`scope/`, `schema/`, `dimensions`, `attribution`, `view`, `query`, `snapshot`).

## Summary

1. The grain-aware aggregation core is genuinely good: one aggregate CTE per measure grain, joined
   at group cardinality with `is not distinct from` plus an explicit `sub_depth` match, is the right
   shape and it is defended by real tests and mutation entries.
2. The injection surface is not values (those are bound) but **identifiers**: every column, dataset
   and derived-dimension name from config is interpolated into SQL with only `"` wrapping and no
   escaping or validation of embedded `"`. One finding is a genuine (config-trusted) injection.
3. Two silent-narrowing / silent-widening risks stand out: an unknown join dataset or unroutable
   join key is skipped with no marker at all, and a view whose measure column name is not a measure
   is silently dropped from the statement.
4. Performance is broadly disciplined (Arrow batches, dictionary cache, stable statement text under
   varying selection sizes), but there is no prepared-statement reuse or plan cache across requeries,
   and the whole SQL string is rebuilt per requery including per-grain CTEs.
5. Clarity is the weakest axis: `compile_view_with_cache` is a single ~570-line function that is
   simultaneously planner, SQL printer and attribution engine, and comment prose is thick with task
   numbers, review-round references and "Phase 4a"-style provenance that CLAUDE.md asks be archived.

---

## Critical

### C1. Identifier interpolation is unescaped, so a config column name containing `"` injects SQL

**Where:** `compile.rs:43-45` (`quoted`), `compile.rs:191-217` (`derived_expr` / `derived_case`),
`compile.rs:653-661`, `compile.rs:745`, `compile.rs:872-883`; `scope_sql.rs:212-226` (`membership`),
`scope_sql.rs:394-436` (`text_column_term`), `scope_sql.rs:437-461` (`selection_clause`),
`scope_sql.rs:757-793` (`derived_membership`), `scope_sql.rs:795-851` (`render_expr`);
`document.rs:74-92`; `distinct.rs:86-99`, `distinct.rs:318-326`.

**What and why:** Every identifier reaches SQL as `format!("\"{name}\"")` with no escaping of an
embedded double quote and no validation upstream. `geode-core`'s schema validator
(`schema/mod.rs:415-618`, `parse_column` at `935-1056`) checks type, role, grain, reserved names
(`RESERVED_COLUMNS` at `schema/mod.rs:409`) and carriability — it never constrains the *character
set* of a column name, and I found no identifier validation anywhere in `schema/mod.rs`, `view.rs`
or `dimensions.rs`. `scope_sql.rs:795-851` (`render_expr`) is the sharpest case: the `column` in
`Expr::Compare`/`Expr::In` comes from the **scope expression grammar**, whose `parse_identifier`
(`scope/expr.rs:~296`) accepts only alphanumerics and `_` — so that path is safe. But
`selection_clause` (`scope_sql.rs:437`) takes `sel.column` from a `DimensionSelection`, and
`dims.base_column()` resolves through `DerivedDimensions`, whose `from` field is an arbitrary TOML
string (`dimensions.rs:44-110` parses `from` with `as_str()` and no character check). A
`dimensions.toml` with `from = 'book" or true --'` produces a predicate that is syntactically valid
and silently true.

**Impact:** Config is trusted-but-shared ("Config is data, shared by default" — PHILOSOPHY §5), and
desk-level config is inherited by every trader. A malformed or hostile `dimensions.toml` /
`datasets.toml` yields a query that widens rather than errors — exactly the "plausible wrong totals"
the crate rules call more serious than an explicit error. It is not remote-attacker-reachable, which
is why this is Critical-by-class rather than an emergency.

**Direction:** One `fn ident(name: &str) -> String` in `store::ddl` that doubles `"` (DuckDB's escape)
and is the only way an identifier becomes SQL text, plus a validator in `geode-core` that refuses a
column/dimension name outside `[A-Za-z_][A-Za-z0-9_]*` at load with a diagnostic. The validator is
the real fix; the escape is the backstop.

### C2. A join whose dataset is unknown or whose key no grain carries is silently dropped

**Where:** `compile.rs:686-702`.

```rust
let Some(joined_ds) = schema.dataset(&join.dataset) else {
    continue;
};
...
if !join.on.iter().all(|k| materialized.contains(k)) {
    continue;
}
let Some(joined_grain) = joined_ds.grains().into_iter()
    .find(|g| carries_all(joined_ds, *g, &join.on, dims))
else {
    continue;
};
```

**What and why:** Three `continue`s. The middle one is legitimate and well-reasoned (the key is not
on the bounded spine, so the columns would be NULL anyway — and `stalest_input.push` at
`compile.rs:704` correctly sits *after* it so a dataset not read does not claim freshness). The
first and third are not: an unknown dataset name and a join key no grain of the joined dataset
carries are both **configuration errors**, and the result is a statement that compiles, runs, and
returns the joined dataset's columns as absent — not NULL-with-a-marker, but missing from `columns`
entirely, so `Snapshot::column_index` returns `None` and the blotter's plan
(`geode-blotter/src/core/plan.rs:~104`, `let index = snapshot.column_index(name)`) paints an empty
column with `attribution` defaulted to `Vec::new()`. `ViewSpec::validate` (`view.rs:324-403`) does
catch an unknown *join dataset* at load — so the first `continue` is partly covered — but it does
**not** validate join keys at all (its own doc says "This does not validate derived SQL, sort keys,
join keys, or column roles").

**Impact:** A typo in `on = [...]` yields a view that silently shows blank reference attributes
forever, with no diagnostic, no `NotApplicable` marker, and no error. The trader sees an instrument
description column that is simply always empty.

**Direction:** Turn both into `compile_error(view, …)` (the function already exists at
`compile.rs:296`), or at minimum attribute the wanted columns `NonAttributable` with a
`ScopeSemantics::NotApplicable` carrying the join dataset name — the vocabulary already exists
(`attribution.rs:50-56`).

---

## Major

### M1. A view column named as a `Measure` that is not a measure disappears from the result

**Where:** `compile.rs:415-424`, with `view.rs:296-313` (`measure_grains`).

```rust
let measures: Vec<&ColumnSpec> = view.columns.iter()
    .filter_map(|c| match c {
        ViewColumn::Measure { name } => ds.column(name),   // None if absent
        _ => None,
    })
    .filter(|c| c.grain() == Some(grain))                  // drops non-measures
    .collect();
```

`ds.column(name)` returning `None` is swallowed, and `c.grain()` is `None` for keys, dimensions,
axes, values and grainless attributes (`schema/column.rs:~117-131`), so any of those named with
`kind = "measure"` is dropped. And `kind` **defaults to `"measure"`** when absent
(`view.rs:514`: `.unwrap_or("measure")`), so a view author who writes `[[v.columns]] name = "book"`
with no `kind` gets a silently absent column. `ViewSpec::validate` checks the name exists somewhere
(`view.rs:352-375`) but never checks the role matches the declared kind.

**Impact:** A column configured in good faith is not in the statement, not in `CompiledQuery::columns`,
and therefore not in the snapshot. The blotter paints it blank. Silent, permanent, and the most
likely authoring mistake given the defaulted `kind`.

**Direction:** Role/kind agreement belongs in `ViewSpec::validate` (it has the schema). Failing that,
`compile_error` here rather than `filter_map`.

### M2. `Aggregate` falls back to `Sum` for a non-measure column reached through the measure path

**Where:** `compile.rs:428-436`.

```rust
let agg = match m.role {
    ColumnRole::Measure { aggregate, .. } => aggregate,
    _ => Aggregate::Sum,
};
```

**What and why:** Unreachable *today* given M1's filter (`c.grain() == Some(grain)` admits only
measures and grain-bearing attributes — and a grain-bearing `Attribute` *does* pass it). So an
`attribute` declared at a grain and named as a measure column reaches this arm and is **summed**.
`schema/column.rs` explicitly documents that "attributes need not be non-numeric", so a numeric
attribute (a spot ref, a notional) is exactly the case: it repeats across its grain's rows and
summing it produces a plausible wrong total.

**Impact:** A plausible wrong number — the failure mode the crate rules single out as worse than an
error. Note `compile.rs:719-747` gets this right for the *join* path, using `any_value` precisely
because attributes "should agree".

**Direction:** Make the fallback `Aggregate::Any`, or refuse. A `_ => Sum` default on a financial
aggregate is the wrong direction of failure.

### M3. Derived-column reference detection is case-sensitive against case-insensitive SQL

**Where:** `compile.rs:101-141` (`referenced_columns`), consumed at `compile.rs:793-816`.

**What and why:** The token scan compares with `tokens.contains(&c.name)` (`compile.rs:135`) — exact
byte equality. DuckDB identifiers are case-insensitive, so a derived column whose SQL writes
`DELTA01 / NPV` (or `Delta01`) against columns named `delta01`/`npv` binds correctly in DuckDB but
matches **no** compiled column here. `referenced.is_empty()` then takes the branch at
`compile.rs:797-800` that assigns `vec![Attribution::Additive; n + 1]` and `ScopeSemantics::Direct`
— the *strongest* claim available, about an expression built from a coarse measure that should have
been blanked. The function's own doc comment (`compile.rs:83-99`) states the invariant this violates:
"A false *negative* hands out `Additive`/`Direct`, the strongest claim available, about an expression
nobody analysed. So every uncertain case here resolves toward matching more." The unbalanced-quote
guard at `compile.rs:127-131` honours that rule; the case comparison quietly breaks it.

**Impact:** A coarse measure exposed at a depth where it does not belong, un-blanked and marked
additive — it will be summed up the tree. No test covers a case-differing derived expression (I
grepped: `compile.rs` has no uppercase-identifier derived test).

**Direction:** Compare `eq_ignore_ascii_case`, both for the token and the column name. Cheap, and it
moves the failure to the safe side.

### M4. The text filter's OR terms and their bound values can be emitted in different orders

**Where:** `scope_sql.rs:640-682`, particularly `term_params.push(bound)` at the end of the loop body
versus `terms.push(...)` inside the `match route(...)` arms.

**What and why:** `compile_scope_cached` is explicitly architected so that values ride with their
clause — the comment at `scope_sql.rs:527-539` narrates the exact defect this prevents ("params were
pushed in *source* order, but the `exists(...)` wrapper holding every finer clause is emitted *last*,
so a finer predicate followed by a direct one transposed their values onto each other's
placeholders"). The text block is the one place that does **not** follow the discipline: it keeps a
flat `term_params: Vec<Value>` parallel to `terms: Vec<String>`. It happens to be correct today
because both pushes occur once per surviving column in the same iteration and each term contributes
exactly one `?` (`text_column_term` at `scope_sql.rs:394-436` returns exactly one bound value). But
the safety is incidental: the `membership(...)` wrapper at `scope_sql.rs:668` embeds `test` inline
rather than deferring it, which is why it survives — and any future term with two placeholders, or
any early `continue` added between the two pushes, transposes a book filter onto an underlying
filter with no error.

**Impact:** Latent. The class of bug is the one this file was already burned by and explicitly
hardened against everywhere else.

**Direction:** Make the text block use the same `Clause = (String, Vec<Value>)` pairing as `Routing`.

### M5. `try_cast` on a stale ENUM produces a NULL indistinguishable from a rolled-up cell

**Where:** `compile.rs:634-662`.

**What and why:** Live grouping columns are cast to their derived ENUM for dictionary-encoded output.
When the dictionary is incomplete, `try_cast` maps the unknown value to NULL so the query does not
fail — and the code comment says so honestly: "That NULL is visually ambiguous with a rolled-up
cell". That ambiguity is the concern. A rolled-up NULL and a stale-dictionary NULL mean opposite
things: "this level does not name a book" versus "this row's book exists but is not in the
dictionary". The row is still present and its measures still real, so the trader sees a real number
attached to a blank dimension.

There is a partly-compensating good decision immediately below: the ORDER BY tie-breakers at
`compile.rs:876-884` deliberately order on `s."g"` (the raw spine varchar), not the ENUM alias,
precisely so a stale ENUM cannot collapse two siblings to one sort key. The comment
(`compile.rs:857-875`) explains it well. The display ambiguity remains.

**Impact:** Known-stale data that is *not* clearly marked, which PHILOSOPHY §3 forbids ("Known-stale
data, clearly marked, is acceptable; ambiguity never is"). Narrow window — ingest refreshes the
dictionary on every publish (`store/ddl.rs:153-190`, `refresh_enum`) — but the window is exactly
"a new book just arrived", the moment it matters.

**Direction:** Either a sentinel the snapshot can distinguish, or drop the ENUM cast for a dataset
whose dictionary does not cover the spine's distinct values (one extra compile-time check against
the cache already in hand).

### M6. The picker's distinct query drops NULL values, so "no book" is unpickable

**Where:** `distinct.rs:104-107`.

```rust
"select value, sum(n)::bigint as n from ({}) u where value is not null group by 1 order by 1"
```

**What and why:** The NULL-book partition is a first-class citizen everywhere else in this crate:
`as_of.rs:17-20` ("The bookless partition is represented by `None`. Ingest retains and reports these
rows, so historical queries must match NULL books too"), `generation_predicate` types it as
`NULL::varchar` (`as_of.rs:140-147`), `membership` uses `is not distinct from` so a NULL key
satisfies its own semi-join (`scope_sql.rs:195-211`), and the README promises "Generation summaries
cover all live/archive pairs, including NULL-book partitions". The picker then hides those rows'
dimension value entirely. A trader cannot select "rows with no book", and the counts shown do not sum
to the dataset.

**Impact:** Rows exist that no picker selection can reach, and the value/count list silently
under-reports the dataset total. Consistent with the rest of the file's care, this looks like an
oversight rather than a decision — there is no comment defending it.

**Direction:** Keep NULL as a distinct pickable entry, or state in the doc comment why the picker
deliberately differs from the storage contract.

### M7. Live provenance reports `latest_gen_id()` — a database-global counter, not this dataset's

**Where:** `read.rs:88-100` and `read.rs:110-127`, against `store/catalog.rs:171-179`.

```rust
AsOf::Live => Freshness {
    dataset: dataset.clone(),
    as_of: catalog.dataset_as_of(dataset, &[])?.map(|t| t.to_rfc3339()),
    generation: catalog.latest_gen_id()?,
},
```

`latest_gen_id` is `select coalesce(max(gen_id), 0) from file_generations` — no dataset predicate.
Every dataset in a joined view gets the same number, and that number is whatever unrelated dataset
published most recently. Meanwhile the as-of arm correctly reports `0` and relies on
`resolved_as_of`, and `dataset_as_of(dataset, &[])` *is* correctly per-dataset and per-book
(`store/catalog.rs:439-453`, taking the `min` over book freshness — the stalest-book rule).

**Impact:** Bounded, because I could find **no consumer** of `Freshness::generation` outside
geode-data (I grepped the blotter and shell for it and found none — the only hit was a comment in
`service.rs:3493`). So this is a wrong number nobody currently reads, which is a latent trap rather
than a live defect. `data-path.md` states "The displayed freshness must reflect the generation
actually selected, not merely the time requested" — the live path does not honour that.

**Direction:** Either scope the lookup to the dataset, or make the field `Option` and report `None`
for live so a future reader cannot be misled.

### M8. `finest_carrying` and `route` disagree about which grain to prefer, without a stated reason

**Where:** `compile.rs:69-79` (`finest_carrying`, `.rev()` → finest) versus `scope_sql.rs:165-168`
(`route`, forward → coarsest) and `distinct.rs:72-77` (coarsest).

**What and why:** Three sites pick a grain that carries a column set, with three different
tie-breaks. Two have documented reasons: `route` picks coarsest "because it is the smallest table"
(`scope_sql.rs:130-134`), `distinct` likewise ("the smallest table that sees every value",
`distinct.rs:71`). `finest_carrying` picks finest, and its doc comment (`compile.rs:61-68`) explains
why it must be *declared* but not why it must be *finest*. For a spine supplying missing depths the
choice is semantically load-bearing: a coarser table has fewer rows per group and a finer one can
have rows a coarser lacks (the cash-only-book case the spine comment at `compile.rs:378-388`
describes). I could not establish that finest is wrong — it is plausibly required — so this is a
clarity/latent-correctness finding, not a defect claim.

**Impact:** Three grain-selection policies a reader must reconcile from scratch; a future edit to one
will not know it diverges from the others deliberately.

**Direction:** Name the policies (`coarsest_carrying` / `finest_carrying`) and state in each doc
comment why that direction is the correct one for that caller. UNVERIFIED whether swapping
`finest_carrying` to coarsest changes any result; a test grouping by a column carried at two grains
where the finer table has extra rows would settle it.

---

## Minor

### N1. `sets.dedup()` only removes *adjacent* duplicate grouping sets

**Where:** `compile.rs:451-467`.

The `(0..=depth)` map produces grouping sets in increasing-prefix order, so equal sets (a grain that
gains no new column at a depth) are adjacent and `dedup` suffices. Correct today, but the invariant
is implicit — the code reads as if it meant "remove duplicates". A one-line comment saying "adjacent
suffices because sets grow monotonically" would pin it, or use a `Vec`-retain against seen.

### N2. `log_refused_result`'s latch makes repeated refusals invisible after the first

**Where:** `pool.rs:390-402`.

Deliberate and documented (one line per worker, not per result, citing "final review, MIN-3"), and
the comment points at `dropped` on the caller's side as the authoritative count. Worth flagging only
because with `query_workers = 1` the latch means a session that refuses ten thousand results logs
once — the honest signal lives entirely in a counter elsewhere.

### N3. The worker polls the queue every 20 ms instead of waiting on the condvar

**Where:** `pool.rs:318-324` (`cvar.wait_timeout(q, Duration::from_millis(20))`).

Every submit already does `cvar.notify_all()` (`pool.rs:241`), so the timeout is a belt-and-braces
wakeup. With N workers this is N lock acquisitions every 20 ms forever, including a fully idle app.
Small, but it is per-worker background churn in a codebase that treats per-frame heap churn as a
reviewable defect.

### N4. `sort` keys are emitted with no NULL ordering and no validation

**Where:** `compile.rs:868-875`; `view.rs:324-403` (validate does not check sort keys).

`order_keys.push(format!("\"{}\" {}", s.column, if s.descending { "desc" } else { "asc" }))` — a sort
column that does not exist is a binder error inside the pool rather than a compile diagnostic (the
`route` function's doc at `scope_sql.rs:126-134` explicitly prefers the latter for scope predicates;
sort does not get the same treatment). No `nulls first/last` anywhere in the crate (I grepped), so
rolled-up NULLs and stale-ENUM NULLs sort wherever DuckDB's default puts them, which differs between
`asc` and `desc`. There is also no abs-sort in the compiler — per the abs-sort handoff that is
client-side in the blotter, so the brief's "abs-sort" item does not apply to this layer.

### N5. `f64` literals in scope expressions bind as `Double` against any numeric column

**Where:** `scope_sql.rs:853-859` (`literal_value`), with `scope/expr.rs:~372-388` (`parse_literal`
parses every non-string non-bool as `f64`).

`strike > 100` on a BIGINT column binds `Value::Double(100.0)`; DuckDB coerces, so it works, but a
literal beyond f64's exact-integer range (>2^53) silently loses precision before it ever reaches SQL.
Narrow, and arguably a grammar issue rather than a compiler one.

### N6. `run_series`'s bin edges are recomputed from `(lo, hi)` rather than read

**Where:** `series.rs:415-441`.

The SQL emits `m.lo, m.hi` on every row and the reader recomputes each edge as
`lo + b * (hi - lo) / n`. The bucket index in SQL (`series.rs:283-289`) uses
`floor((w.v - m.lo) / (m.hi - m.lo) * k) + 1`. The two arithmetics are equivalent in exact math but
not in floating point, so a value exactly on an internal edge can be reported in bin `k` while the
edge label says it belongs in `k+1`. Cosmetic for a histogram; worth a comment that the edges are
labels, not the boundaries the counts were computed with.

### N7. `nan_if_null` uses NaN as the gap marker, which every downstream must remember

**Where:** `series.rs:363-369`.

Documented as a deliberate contract ("a gap must not arrive as a zero a chart would draw, nor as a
number any later arithmetic could absorb") and it is the right call for a dense `Vec<f64>`. Flagged
only because NaN propagates silently through any arithmetic a future consumer adds — the invariant
lives in prose, not in the type.

### N8. Comment prose carries task numbers, review rounds and phase provenance

**Where:** pervasive. Counted non-module-doc citations: `scope_sql.rs` 27, `pool.rs` 19,
`distinct.rs` 15, `catalog.rs` 7, `series.rs` 5, `document.rs` 4, `mod.rs` 2.
Examples: `mod.rs:7-12` ("five review rounds found defects … the single most repeated defect class in
phase 2b"), `scope_sql.rs:46-56` ("Phase 4a's as-of baseline fix"), `pool.rs:31-36` ("Phase 4b
follow-up, Task 1"), `pool.rs:390-393` ("final review, MIN-3"), `distinct.rs` ("market-data spec
§4.5, Part 2 Task 1").

CLAUDE.md is explicit: "A code comment should state the local invariant and failure it prevents; it
should not require a task number or spec section to make sense." Most of these comments *do* state
the invariant and the failure — they are genuinely excellent on substance — and then append a
provenance tag that will outlive its referent. `compile.rs` and `as_of.rs` and `read.rs` score zero
and read better for it; they are the model.

### N9. `compile_view_with_cache` is one ~570-line function doing four jobs

**Where:** `compile.rs:335-903`.

Planner (grain selection, routing, spine assembly), SQL printer, attribution engine and provenance
recorder in one body, with `#[allow(clippy::too_many_arguments)]`. There is no AST → plan → SQL
pipeline; the plan exists only as local `Vec<String>`s (`ctes`, `selects`, `joins`, `agg_joins`,
`agg_selects`, `agg_columns`, `spine_sources`) whose ordering relationships are maintained by
comments. The brief asks whether the compiler is a clean pipeline: it is not, it is a well-commented
straight line. Given how much correctness rides on the *order* things are pushed into those vectors
(see M4), a typed intermediate (`struct Plan { ctes: Vec<Cte>, … }` where a `Cte` owns its params)
would make the params/placeholder correspondence unrepresentable-if-wrong rather than
maintained-by-discipline.

### N10. No prepared-statement reuse or plan cache across requeries

**Where:** `pool.rs:432-454` (`run_snapshot` calls `conn.prepare(&compiled.sql)` per query);
`read.rs:74-140` (recompiles from scratch per request). No `prepare_cached` anywhere in the crate
(grepped).

The statement *text* is deliberately kept stable across selection sizes — `string_split` over one
delimiter-joined varchar rather than an N-placeholder `IN` list (`scope_sql.rs:1-14`, `437-461`),
explicitly "so the prepared plan stays cacheable". That care is then not cashed in: nothing caches
the prepared statement, so DuckDB re-plans every requery. Measured requery is 2.51 ms against a
50 ms budget (`performance.md:69`), so there is headroom and this is correctly not a priority — but
the stated reason for the `string_split` design is currently unrealised.

### N11. Per-requery string building is unbounded by design

**Where:** `compile.rs:335-903` builds every CTE, select, join and order key with `format!` on each
call; `as_of.rs:130-165` (`generation_predicate`) builds an ID list plus one `(batch, book, gen_id,
source_time)` tuple **per partition** as SQL literal text.

For a dataset with many books × many batches the as-of predicate grows linearly in partitions, as
literal text, on every historical requery. The doc comment defends the shape (ID list for row-group
pruning, tuple match for exact identity, source time for legacy reused IDs) and the reasoning is
sound. The cost is only that it is rebuilt each time and inlined as literals rather than bound —
which is also what makes it prunable, so the tradeoff is real. No buffer reuse anywhere in the
compile path; PHILOSOPHY §6 says "buffers are reused across frames and refreshes".

### N12. `strip_sql_comments` collects the whole expression into `Vec<char>`

**Where:** `compile.rs:143-181`.

`let bytes: Vec<char> = sql.chars().collect()` allocates a 4-bytes-per-char vector for every derived
expression on every compile, then builds a second `String`. Derived expressions are short so this is
trivial in absolute terms; it is a per-requery allocation in a path the philosophy asks be
allocation-free. A single forward pass over `char_indices` needs neither allocation.

---

## Ideas

### I1. Golden-test the generated SQL

There is no golden/snapshot test of any generated statement anywhere in `query/` (checked all test
modules). Tests assert on *behaviour* — row values, `matches("exists").count()`, param counts,
`DictionaryCache::lookups` — which is the right primary strategy and catches semantic regressions
well. But a golden SQL corpus (one file per interesting view/scope/era combination) would make
structural regressions legible in review: a reviewer could see "this diff changed the join from
`is not distinct from` to `=`" without reconstructing it from a failing assertion. It would also
document the contract the brief asks about, which currently has to be inferred from 2,700 lines of
tests.

### I2. Property-test the compiler the way `scope_sql` property-tests the dictionary

`scope_sql.rs:1883` and `2062` have two `proptest!` blocks — `dictionary_and_row_scan_agree_for_any_needle`
and `compile_scope_and_compile_scope_cached_agree` — and they are the strongest tests in the
directory because each has an independent oracle. The compiler has the same oracle structure
available and unused: for any view/scope, **the sum of a level's children must equal the parent**
wherever attribution is `Additive`. That is one property, checkable against arbitrary generated
schemas and data, and it is precisely the "plausible wrong totals" invariant the whole grain split
exists to protect. `as_of.rs:76-113` already demonstrates the pattern with `resolve_from_tables` as a
scan-based oracle for summary-based resolution.

### I3. Give the column-name collision between derived and base a compile-time answer

`ViewSpec::validate` catches a derived *dimension* shadowing a real column and says why it matters
("the group-by resolves to whichever the compiler reaches first, so the answer is quietly the wrong
one rather than absent" — `view.rs:378-390`). That reasoning applies verbatim to a derived *column*
(`ViewColumn::Derived`) whose `name` equals a base column's, which nothing checks: `compile.rs:816`
pushes `{expr} as "{name}"` into the same select list that already contains `"{name}"`, and
`referenced_columns` will then match the derived column against itself on a later derived column.
The comment at `compile.rs:807-811` shows awareness of the adjacent problem ("A bare input name in
this SELECT resolves to the joined aggregate's column before its neighboring masked alias"). Worth
the same load-time refusal.

### I4. Make `ScopeSemantics::NotApplicable` reachable from the view compiler

`attribution.rs:50-56` defines `NotApplicable` for "some scope selection named a dimension this
dataset does not have at all, so it was dropped" and `meet` handles it as weakest
(`attribution.rs:73-87`). Only `distinct.rs` produces it (via `applicable_to` at
`distinct.rs:207`) — and even there the dropped names are discarded (`let (scope, _dropped) = …`).
`compile_view` never produces it, yet C2's silently-dropped join is exactly the situation it
describes. The vocabulary is built and the blotter already renders it
(`geode-blotter/src/core/plan.rs`, "`NotApplicable` paints the same marker as `SemiJoined`"); the
producers are missing.

### I5. Extract the era/relation/generation trio into a `Read` type

`Era` is re-exported from `mod.rs:5-21` with a comment explaining that five review rounds found
defects at sites reaching for `TableKind::Live` directly, so "leaving it reachable only via
`scope_sql` made the wrong thing the convenient one". That is the right instinct and it worked. The
next step is that `document.rs:100-130` and `distinct.rs:186-201` each **rebuild** the both-sides
union by hand (`"(select * from {} union all select * from {})"`) rather than going through
`Era::relation` — because the document family has no `Grain` and `relation` takes one. Two hand-built
copies of the one rule `Era` exists to centralise. A `TablePair`-based `relation` (the pair type
already exists at `store/ddl.rs:~52-78` precisely so "the two families go through one door") would
close it.

---

## Systemic patterns

**The failure-direction discipline is real and mostly honoured.** "Fail toward the weakest claim" is
stated at `compile.rs:83-99` and enforced at `compile.rs:127-131`; "a needle that matches nothing
selects nothing, never everything" is stated and enforced in three places
(`scope_sql.rs:672-676`, `distinct.rs:271-275`, and the `nothing()` early return at
`scope_sql.rs:519-525`); "an unreadable row is an error, not a smaller answer" is stated and tested
twice in `as_of.rs`. Where the code breaks, it breaks by *omission* of the rule at one site (M3's
case comparison, C2's `continue`s, M2's `Sum` default), never by contradicting it.

**Values are bound; identifiers are interpolated.** This split is consistent and deliberate — the
module docs say so (`as_of.rs:115-118`: "Catalog strings are escaped as SQL literals; user-supplied
scope values use bound parameters elsewhere"). The literal escaping that *is* done is correct
(`compile.rs:183-189` `sql_literal` doubles quotes; `as_of.rs:140-152` doubles quotes on batch and
book). Identifiers got no equivalent, and `geode-core` never constrains them. That is C1.

**Parallel vectors maintained by comment.** `compile.rs`'s seven local `Vec`s and `scope_sql.rs`'s
text-block `terms`/`term_params` both rely on push-order discipline. `scope_sql.rs` learned this
lesson once (the `Clause` tuple, `scope_sql.rs:527-539`) and applied it everywhere except the text
block. `compile.rs` has not learned it yet — `params.extend(grain_scope.params)` at `compile.rs:493`
is correct only because it immediately follows the matching `ctes.push`.

**Comments are the design documents.** Substance is outstanding — most non-obvious decision has a
comment naming the invariant *and* the wrong behaviour it prevents, often with the measured cost
(`scope_sql.rs:615-639` gives ~65 ms for the rejected subquery form). The recurring defect is
appending provenance (task numbers, review rounds, phase names) that CLAUDE.md asks be archived, and
occasionally length: `scope_sql.rs:36-64` is a 29-line doc comment on a two-field struct.

**Test strategy is behaviour-first with oracle-based tests where available.** ~2,700 of `compile.rs`'s
3,684 lines are tests, against real DuckDB with real published generations, and the test *names* are
behaviour claims ("a_coarse_measure_does_not_double_count_at_any_level",
"a_null_reference_key_does_not_attach_to_every_rolled_up_row"). 92 mutation entries cover this
directory (compile 22, scope_sql 19, distinct 16, pool 10, series 10, as_of 6, document 6, read 3).
Gaps are specific rather than systemic: no case-differing derived expression (M3), no `Expr::Or` /
`Expr::Not` routing test in the compiler (only `scope_sql.rs:1277`, and that one asserts it *compiles*
rather than what it means), no sort-key test at all (N4), no NULL-value picker test (M6).

---

## What is done well

- **The grain split is the right architecture and it is defended.** One CTE per measure grain, each
  grouping by its own projected prefixes, joined on `is not distinct from` plus an explicit
  `sub_depth` equality (`compile.rs:401-577`). The `sub_depth` carry exists because "matching on key
  values alone cannot tell a rolled-up NULL from a NULL that is really in the data"
  (`compile.rs:467-472`) — that is the subtle case, it is identified, and
  `a_real_null_in_a_grouping_column_does_not_fan_out_the_tree` tests it.

- **The spine is assembled from the aggregates, not scanned.** `compile.rs:378-395` explains why: a
  cash-only book has position rows and no underlying rows, so a spine scanned from the finest table
  left it in the grand total and on no row beneath it, and children did not sum to their parent.
  `a_cash_only_position_has_a_row_at_the_lhu_level` and
  `a_scope_selecting_nothing_still_yields_the_grand_total_row` cover it.

- **One era for the whole statement.** `Era` (`scope_sql.rs:65-119`) is the single applier of the
  generation predicate, on both live and archive, including a `Live` era that happens to carry one —
  and the doc comment explains that an arm silently dropping a handed-in predicate "would be a
  trapdoor for the next caller". Semi-join probes read the caller's era (`scope_sql.rs:212-226`), so
  a probe cannot mix today's data into a historical answer. `an_as_of_query_reads_only_the_archive_even_through_a_semi_join`
  is exactly the right test.

- **Per-dataset as-of resolution with honest provenance.** Each joined dataset resolves its own
  generations (`compile.rs:716-733`) and records its own oldest selected source time, because
  "Reporting the requested instant for both sides would conceal a stale input"
  (`compile.rs:36-40`). `an_as_of_join_labels_each_side_with_the_instant_it_actually_read` tests it
  and a mutation entry guards it.

- **The total row order.** `compile.rs:845-884`: shallowest-first so a parent precedes its children,
  then grouping columns as tie-breakers so "a tile that requeries every few seconds" does not
  reshuffle unchanged rows, ordered on the raw spine column rather than the ENUM alias so a stale
  dictionary cannot collapse two siblings. Three distinct failures anticipated in one ORDER BY, each
  with a test.

- **The dictionary cache's scope is exactly right, and its danger is documented.** `DictionaryCache`
  (`scope_sql.rs:295-390`) is per-statement, `pub(crate)`, not re-exported, and its doc comment
  states in bold why holding one longer would silently serve a retired book as live. The two
  proptests proving cached and uncached agree are the correct way to prove a cache.

- **The pool's concurrency reasoning.** `next_id` lives inside the mutex so "allocating outside the
  lock is not expressible" (`pool.rs:131-140`, noting the race was caught on 2 runs in 8 — making it
  unrepresentable beats testing for it). Stale-check and sink call under one lock. Self-inflicted
  interrupts from both `cancel` *and* `shutdown` are distinguished from genuine failures
  (`pool.rs:350-360`) — and the comment notes shutdown was "the same defect `cancel` was fixed for,
  left standing on the neighbouring path", which is the mechanism-vs-instance discipline applied.

- **Columnar end to end.** `run_snapshot` (`pool.rs:432-454`) reads Arrow `RecordBatch`es and hands
  them to `Snapshot::from_batches`, which concatenates preserving dictionary encoding
  (`snapshot.rs:95-177`) and validates that `meta[i]` describes batch column `i`
  (`snapshot.rs:411-427`) rather than trusting it. No row objects are materialised. `f64_in`'s
  Decimal128 arm (`snapshot.rs:246-276`) exists because `sum(BIGINT)` is HUGEINT and without it every
  such cell read blank — "under §6.3 a blank cell is a positive claim", so the wrong-direction
  failure was identified and closed, with `a_summed_bigint_comes_back_as_a_decimal` testing it.

- **`compile_document` refuses to reuse the view compiler.** "Deliberately not a view: there is no
  grouping, no aggregation and no scope — the key IS the predicate — so the tree compiler has nothing
  to add and everything to get wrong" (`document.rs:1-5`). Choosing *not* to generalise, with the
  reason stated, is the harder and better call.

- **`geode-core` stays pure.** `attribution_of` (`attribution.rs:115-155`) decides additivity from the
  schema alone with no I/O, which is why it lives in core beside the grain vocabulary. `Grain`'s
  `dimension_key_columns` (`schema/grain.rs:64-73`) encodes that the pair table's canonicalised
  `least(u1,u2)` must never be treated as an underlying dimension, and both the grain tests and
  `the_underlying_level_shows_every_underlying_not_the_first_of_each_pair` hold the line. This is the
  "lens, not a brain" boundary observed precisely: the compiler shapes views and performs no
  financial arithmetic anywhere.
