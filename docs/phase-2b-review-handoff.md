# Phase 2b review handoff

Written 2026-08-30, at branch `phase-2b-query-path`, for whoever picks up
the next review/fix round. Round 5's findings are at the top; the rest is
the state of the world after them.

## Where this stands

Phase 2b (the query path) is feature-complete and has been through **five
review rounds**. Each round found Critical, execution-verified defects:

| Round | Found | Fixed in |
|---|---|---|
| 1 | 7 Criticals — semi-join scoping, NULL fan-out, join multiplication, NULL join keys, depth-0 joined views, as-of on joined datasets, derived dimensions never materialized | `a7a1cb0` |
| 2 | 2 Criticals — both *siblings* of round 1's, in a code path round 1's fix had unblocked | `8ac80e4` |
| 3 | 3 Criticals — param/clause desync, era-blind ENUM cast, NULL books dropped from history | `e70c2db` |
| 4 | 2 Criticals — generations resolved from one grain, `is_finer` wrong for same-grain measures | `2029e91` |
| 5 | 7 Criticals — pair-grain `underlying_ref` treated as the underlying dimension (spine and scope), as-of blind to the current generation, spine dropped partitions absent from the finest table, text filter silently skipped finer columns, measure/attribute predicates probed the wrong table, same-instant generations resolved nondeterministically, bookless partition never replaced | this round |

All five rounds are green on `cargo test --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo fmt --check`, and
`cargo bench --workspace --no-run`.

## Round 5: what was found, and how

Every defect below was reached by **execution first**: a probe test asserting
the correct answer, run against the unmodified code, observed failing, then
fixed, then turned into a permanent test. Mutations in
`scripts/mutation-check.sh` revert each fix individually and confirm the new
tests see it.

### 1. The pair grain's `underlying_ref` is not the underlying (the worst one)

`measures_underlying_pair` carries `(underlying_ref, underlying2_ref)` as
the canonical `(least, greatest)` of a pair (§3.3). The compiler treated
`underlying_ref` on that table as the same column it is everywhere else.
On the real desk schema — all four grains declared — the pair table is the
*finest* grain, so it became the spine of every tree, and the "direct"
target of every underlying predicate. Two silent consequences:

- **Scoping to an underlying dropped every position where that underlying
  sorted second in its pair.** Probe P8: `underlying_ref = 'SPX'` against a
  worst-of over RUT/SPX returned a lone total row with a blank PnL. On a
  worst-of over NDX/RUT/SPX, SPX sorts second in every pair.
- **The underlying level of the tree showed only the `least` of each
  pair.** The benchmark fixture is the measurement: after the fix the
  unscoped 1M-row tree has **398,085 → 729,466 rows** — the old spine was
  omitting roughly 45% of the tree, and those rows' greeks joined to nothing
  while still counting in the total, so children did not sum to parents.

Fix: `Grain::dimension_key_columns()` in `geode-core` — the key columns that
carry their dimension meaning; for the pair grain that is the instrument
key. The attribution rule, the spine, aggregate ownership, scope routing
and cross-dataset join-grain selection all use it. A side effect is that
cross gamma is now `NonAttributable` at an underlying-level grouping, which
is what spec §6.3 says and was not what the code did.

### 2. As-of could not see the current generation

The publish transaction moves the *outgoing* generation to the archive and
never copies the incoming one (§4.3). So the generation a partition holds
now is in `_live` and nowhere else — and `resolve_generations` read only
`_archive`. Probe P2: as-of any instant after a partition's latest
publish answered with its *previous* generation (100 instead of 7), and a
partition published only once was absent from every historical answer.
Same class as round 1/2/3/4's era defects; nobody had asked "as of an hour
ago". Also silently blanked every reference column in an as-of join whose
reference dataset had only ever been published once.

Fix: `Era::relation()` — as-of reads `archive union all live` under the
generation predicate; `resolve_generations` resolves across both;
`as_of_bounds` starts at the oldest generation anywhere. The generation
predicate now names `source_time` as well as `gen_id`, because `gen_id` is
`max + 1` over the catalog and allocated before the catalog row is written
(a load that publishes then fails to record leaves an id the next load
reuses).

### 3. Same-instant generations resolved nondeterministically

A corrected republish keeps its source time (§4.4), so the archive holds
two generations of one partition at one instant. `row_number() over (…
order by source_time desc)` picked whichever came back first: probe P3
observed both across twenty runs, and `retention.rs` could keep the one
`as_of.rs` dropped. Fix: `, gen_id desc` in both. The two harness entries
for this are caught probabilistically (the tests loop).

### 4. The spine dropped partitions absent from the finest table

A cash-only position (trading PnL, no greeks) was in the grand total and on
no row beneath it (probe P4: total 907, LHU rows summing to 7). The spine
was a scan of the finest grain's table. Fix: the spine is assembled from
the aggregates — each grain's own levels, for the depths it carries in
full, unioned with a constant grand-total row — and a table scan is only
emitted for depths no measure grain carries. This also removed the
depth-0 full scan the previous handoff listed as backlog.

### 5. Text filter silently skipped finer textual columns

At position grain the text filter dropped `underlying_ref ilike …` because
the column is not there, so the greeks were filtered and the PnL beside
them was not, marked `Direct` (probe P6). Fix: each textual column routes
on its own; one this grain lacks becomes a membership term inside the OR,
marked `SemiJoined`. LIKE wildcards in the typed text are now escaped too
(`50_` no longer matches `500`).

### 6. Measure and attribute predicates probed the wrong table

Every "finer" clause was tested against one probe — the spine's grain —
which does not carry other grains' measures or attributes. `delta01 > 5`
from position grain was a binder error the moment the spine was the pair
table (probe P5), i.e. on the real schema, for every measure but one.
Fix: `scope_sql::route` — each clause goes to the coarsest declared grain
that carries every column it names; top-level `and` terms route
separately; the shared keys are the coarser grain's dimension keys; the
result is `SemiJoined` only when those keys do not pin the probe's
identity (an instrument attribute tested from underlying grain is
`Direct`). Unknown columns and unroutable clauses fail at compile time.

### 7. The bookless partition was never replaced

`partition_predicate` emitted `book = '…'`, which matches no NULL, and
ingest built partitions from non-NULL books only. Every republish appended
another copy of the bookless rows (probe P7). Fix: `Partition.book` is
`Option<String>`, `book is null` in the predicate, ingest adds the `None`
partition when it reports unattributed rows.

### Also changed

- As-of freshness reports the **oldest** resolved partition per dataset,
  not the newest — §4.5's rule, the same one live freshness applies.
  Round 4 chose newest deliberately; this is a reversal, flagged so it can
  be reversed back if the spec is read differently.
- `service.rs::a_historical_result_is_labelled_with_the_data_it_actually_read`
  was vacuous twice over: it opened a second `Connection` on the file (a
  separate database instance whose writes the service never saw) and
  `.ok()`ed an insert that failed anyway on column count. It now writes
  through the service's store and asserts the exact label.
- The unreachable `else 0` arm of the `sub_depth` CASE is `else -1`.

### Dismissed after execution

- DuckDB's session `TimeZone` is the machine's local zone
  (`America/New_York` here), but chrono parameters bind as varchar with an
  explicit `+00:00` offset, so as-of comparisons are unaffected.

## The thing to understand before starting

**The recurring failure mode is not bad reasoning, it is an unreachable
fixture.** Every round fixed exactly what was pointed at, and the next
round found the same *class* somewhere adjacent. Round 5's biggest finding
was reachable only with the pair grain declared — which every real
schema and the benchmark do, and no compiler test did.

So: **do not trust a green suite here, and do not trust a test because it
looks thorough.** Break the code and see if anything notices.

## Use the mutation harness

`scripts/mutation-check.sh` — 27 mutations across the compiler, scope
lowering, as-of routing, publish, and the grain vocabulary. Run it after
any change to those files:

```sh
zsh scripts/mutation-check.sh
```

A `SURVIVED` line means a branch no test can see. **The harness is not
complete** — it covers what five rounds happened to touch. Adding
mutations for whatever you work on is the highest-value thing you can do
here, and probably higher-value than another read-through.

## Defect classes that have recurred (check these first)

1. **Era / as-of routing.** Every site that names a relation or builds a
   WHERE clause must go through `Era::relation` and consult
   `era.generations`. Missed historically in: cross-dataset joins (r1),
   the semi-join probe (r2), the ENUM cast (r3), generation resolution
   grain (r4), the live half of the relation (r5). Known remaining:
   `DataService::freshness()` is unconditionally live (no caller today).
2. **NULL-unsafe matching.** For every `=` / `is not distinct from` on
   keys, decide whether NULL there is a rolled-up placeholder (`=`) or a
   real value on both sides (`is not distinct from`). Known remaining:
   `Catalog::live_source_time` and `book_freshness` join `file_books` by
   `book`, so the bookless partition has no freshness of its own and rides
   its file's other books through the backfill guard.
3. **Derived dimensions (§6.8).** Every path consuming a column name can
   receive a derived one. Known remaining: `ViewSpec::validate` and
   `Scope::validate` both reject derived names as unknown columns.
4. **Column names that mean different things at different grains** (new
   in r5). `Grain::key_columns()` is the physical key; anything deciding
   what a column *means* must use `dimension_key_columns()`. The remaining
   `key_columns()` callers are DDL, the split, the ENUM refresh and schema
   validation, all physical.

## Deferred backlog (known, not yet fixed)

Nothing below is believed to block merge, but none of it is free.

**Concurrency — `query/pool.rs`:** a panic in `run_one` leaves the view in
`running` forever, permanently wedging that view; the stale-check and the
send are not atomic, so a superseded result can still be delivered;
`cancel` delivers a spurious `Interrupted` the UI renders as an error; the
query id is allocated outside the lock, so concurrent submits can leave
the older request pending; `submit` ignores `shutdown`; a mid-loop
`expect` on worker spawn detaches already-spawned workers. Also
`service.rs` field order drops `_store` before the pool's workers join.

**Snapshot — `geode-core/src/snapshot.rs`:** `f64_column` returns
`.values()`, discarding the null bitmap, so every cell the compiler
deliberately blanks reads back as `0.0` — add `f64_value(name, row) ->
Option<f64>`. `dict_column` hardcodes `UInt8` keys, so a dimension with
more than 255 values falls through both it and `str_column`. `dict_value`
ignores the null bitmap.

**Ingest — `gen_id` allocation:** `Catalog::next_gen_id` is `max + 1`,
peeked before the catalog row is written; a publish that then fails to
record reuses the id on the next load. The generation predicate now
tolerates this (it names `source_time` too), but a sequence would remove
the hazard.

**Compiler — `query/compile.rs`:** no `ORDER BY` is emitted when
`view.sort` is empty, so "shallowest first, parent before children" does
not actually hold. Derived view columns are unconditionally marked
`Additive`/`Direct`. Grouping by an attribute (`model_code`, which §6.3
names) is a compile error — attributes are not dimension keys of any grain.
Two references to each aggregate CTE (spine and join); DuckDB handled it
within budget, but `as materialized` is the lever if a future shape does
not.

**Validation:** `ViewSpec::validate` and `Scope::validate` are never
called from `DataService::open`. Scope errors now surface at compile time
as `StoreError::Sql` rather than as a `Diagnostic` (§10.1).

**Smaller:** the live ENUM cast has no value-safety guard (`try_cast`);
`generation_predicate` inlines one term per partition (now with a source
time each), so statement text grows with partition count and defeats plan
caching; `Scope::columns()` returns `[]` for a contradiction; `Era::live()`
is test-only.

## Fixture gaps (these predict the next round's findings)

Now covered: the pair grain declared (`pair_fixture`), a live query against
the hostile store, a cash-only partition below the total, same-instant
generations, a scope expression on an attribute and on a measure of
another grain, a textual column finer than the grain, a bookless partition
through the real load path, an as-of join against a once-published
reference dataset.

Still **not** represented anywhere:

- more than 256 distinct values in a derived ENUM (the `UInt8` cliff)
- a dimension value present at a finer grain but absent from the table its
  ENUM was built from
- a view grouped by a derived dimension whose `from` column is absent at
  one of the view's measure grains
- an as-of query over a *joined* dataset whose two sides resolve to
  different instants (the stalest-input label is only tested with one
  dataset resolving)
- the real ingest path producing a corrected republish and then a third
  generation, so the tie sits between archive and live rather than within
  the archive

## Conventions

- Worktree at `.claude/worktrees/phase-2b-query-path`; run everything
  from there, never `cd` to the main checkout.
- Main has moved to `08db542` (move-tile → `ctrl+alt+arrows`), which edits
  `defaults.rs`. This branch also edits it (adds `mod+shift+d` /
  `data::toggle_probe`). Round 2 verified: different hunks, no collision,
  git merges both cleanly.
- The throwaway probe (`geode-shell::dataprobe` + `geode-app/src/probe.rs`)
  is opt-in via `GEODE_PROBE_DIR` and gets deleted by the blotter in
  phase 3. To exercise it end to end:
  `cargo run -p geode-demo-data --example emit -- <dir> 100000`, then set
  `GEODE_PROBE_DIR=<dir>` and a `GEODE_DESK_CONFIG` dir containing
  `datasets.toml` and `views.toml`.
- Governing docs: `docs/superpowers/specs/2026-08-30-geode-phase-2-data-design.md`
  (all `§` refs in code point here), `docs/PHILOSOPHY.md`, `CLAUDE.md`
  (crate layering: `geode-shell` and `geode-data` must never depend on
  each other; only `geode-app` sees both), `docs/perf.md`.
