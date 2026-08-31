# Phase 3 prerequisites

Work that Phase 2b deliberately left undone, scoped for a branch to be
taken **before** the blotter. Written 2026-08-31 at `9c03941`.

## Status (2026-08-31, branch `worktree-phase-3-prerequisites`)

**The three blockers are done, and so is all of §2.** Still open: §3
(`gen_id` sequence), §4 (validation wiring), and every §5 item except the
`ORDER BY` one. Those are unchanged and still worth doing.

Two things this branch found that were not in the list below:

- **`depth_of_row` returned `None` for every real query result.** DuckDB
  emits the narrowest integer that fits, so `row_depth` arrives as
  `Int32`, and `i64_column` only matched `Int64`. The probe does
  `depth_of_row(row).unwrap_or(0)`, so every row was treated as the grand
  total and read `attribution_by_depth[0]` — the `NonAttributable` and
  `DeterminedNonAdditive` markers were wrong for the whole tree. Invisible
  because every fixture built depth with `TestColumn::I64`. Fixed with
  `i64_value`, which reads any width. **This is the same class as §1: an
  accessor tested against a fixture rather than against what DuckDB
  produces. Assume there are more, and check any accessor that names a
  concrete Arrow type.**
- **The comment on `concat_preserving_dictionaries` was wrong.** It
  claimed arrow's `concat` appends dictionaries and overflows the 8-bit
  key space. Measured at arrow 58.4.0 it unifies them, and where the union
  genuinely will not fit it returns an error rather than corrupting. The
  fast path is an optimization, not a correctness guard; the comment now
  says so and records the measurement.

The mutation harness was corrupting the source tree and is fixed —
see the note at the end of this file before running it.

Phase 2b was five review rounds of correctness work on the *query* path.
Everything below is the *read* path — how a `Snapshot` is consumed — plus
two durability items. None of it blocks Phase 2b, and all of it blocks a
blotter that renders real numbers, which is why it is a prerequisite
rather than backlog.

## What actually blocks the blotter

Three items, in the order the blotter will hit them. Do these first; the
rest can ride along with Phase 3 work.

1. **`Snapshot` cannot express a null (§1).** The blotter's very first
   render is wrong without it: every deliberately-blanked cell reads as
   `0.0`. Nothing downstream can compensate, because the information is
   already gone by the time a module sees it. Fix `f64_value` first, then
   the dictionary width — a real underlying list exceeds 255 values, so
   that one bites on the first realistic dataset too.
2. **`ORDER BY` is not emitted when a view declares no sort (§5).** The
   blotter's flatten walk assumes parent-before-children; the compiler
   does not currently guarantee it. This is a one-line fix and a test,
   and it is load-bearing for the tree the whole phase is about.
3. **The pool is not panic-safe (§2).** A panic in a worker permanently
   wedges that view — the tile stops updating with no error, for the rest
   of the session. Survivable in a probe that requeries every 5s;
   unacceptable in a blotter someone leaves open all day.

Everything else — `gen_id` allocation, validation wiring, the smaller
items — is real but does not stop the blotter being built correctly. Fix
them when the surrounding code is already open.

**Before starting any of it:** run `zsh scripts/mutation-check.sh` to
confirm the existing entries still pass, and add an entry for each
behaviour you change. Five review rounds of evidence say a green suite
here proves very little on its own.

**The harness had a defect that corrupted the source tree**, fixed on this
branch. It backed every file up to a single hardcoded `/tmp/mutate.bak`
and `set -e` aborted before restoring, so an interrupted run left its
mutation in the tree — one was found uncommitted at the start of this
branch — and two concurrent runs restored each other's backup over the
wrong file, leaving one checkout with the contents of `scope_sql.rs`
inside `compile.rs`. It now uses a per-run backup, a lock per checkout,
and a trap that restores on any exit. `run_mutation` also takes the
package whose tests should see the mutation (a `geode-core` mutation
checked with `-p geode-data` reports "caught" on unrelated tests), and
the script takes a substring to run a subset while iterating.

**Commit before you mutate.** Restoring a mutated file with `git checkout`
discards uncommitted work along with it.

## 1. `Snapshot` cannot express a null (blocks the blotter) — DONE

`geode-core/src/snapshot.rs`. All four bullets fixed. `f64_value` and
`text_value` are the accessors a renderer should use; `f64_column` and
`i64_column` are documented as bulk paths that cannot express a NULL.
The era question was answered by making the accessors era-agnostic
(`text_value` reads a dimension under either encoding) rather than by
recording the encoding in `ColumnMeta`.

- **`f64_column` discards the null bitmap.** It returns `.values()`, the
  raw buffer. Every cell the compiler deliberately blanks —
  `NonAttributable`, the whole point of §6.3 — reads back as `0.0`, and
  no accessor can tell otherwise. A blotter would render a confident zero
  where the honest answer is "this number does not belong to this row".
  Add `f64_value(name, row) -> Option<f64>` honouring `is_null`, and
  route the probe tile's `cell_text` through it.
- **`dict_column` hardcodes `DictionaryArray<UInt8Type>`.** DuckDB
  promotes an ENUM to `Dictionary(UInt16, Utf8)` above 255 values
  (verified: 200 → UInt8, 300 → UInt16). Any dimension with more than 255
  distinct values — every real underlying list — falls through both
  `dict_value` *and* `str_column` and renders blank. Match on the key
  width, and extend `concat_preserving_dictionaries` likewise.
- **`dict_value` ignores the null bitmap**, so a NULL dimension cell
  returns dictionary entry 0: the grand-total row displays a real book
  name.
- A snapshot's column *type* now depends on the era (as-of skips ENUM
  interning) while `ColumnMeta` does not record it. A blotter caching
  dictionary codes across a live↔as-of toggle will get `None` with no
  signal. Either record the encoding in `ColumnMeta` or make the
  accessors era-agnostic.

## 2. The query pool is not panic-safe (blocks a long-running session) — DONE

`geode-data/src/query/pool.rs`. Every bullet fixed. The pool takes the
per-request work as a function pointer so a test can inject a panicking
one; there is no SQL that makes `run_one` panic, and panic-safety is the
property most worth testing here.

Two of these have no mutation entry, deliberately: allocating the id
under the lock, and holding the lock across the stale check and the send.
Both are races whose mutation is only observable on an interleaving a
test cannot force, so an entry would report `SURVIVED` whether the code
is right or wrong. They are argued in comments at the site instead.

- A panic in `run_one` leaves the view's entry in `running` forever,
  permanently wedging that view and silently shrinking the pool. Remove
  the entry from a `Drop` guard, or `catch_unwind`.
- The stale-check and the `tx.send` are not atomic — the lock is released
  between them — so a newer submit landing in that window still lets the
  older result be delivered, defeating §7.3's ordering guarantee.
- `cancel` removes the pending entry and then interrupts, so the worker
  sees `stale == false` and delivers the resulting `Interrupted` as a
  query failure the UI renders as an error.
- `next_id.fetch_add` runs before the lock is taken, so two concurrent
  submits for one view can insert out of id order and leave the *older*
  request pending.
- `submit` ignores `q.shutdown`: a post-shutdown request is inserted,
  never runs, and the caller blocks until timeout.
- A mid-loop `expect` on worker spawn panics with earlier workers already
  running and no shutdown flag set, detaching them.
- `service.rs` field order drops `_store` (and its database handle)
  before `QueryPool`'s workers are joined.

## 3. `gen_id` allocation can collide

`geode-data/src/store/catalog.rs`, `next_gen_id()`.

`gen_id` is `max(gen_id) + 1` over `file_generations`, computed *before*
the catalog row is written. A load that publishes and then fails to
record leaves an id the next load reuses, so two generations of one
partition can share an id. Round 5 made the generation predicate tolerate
this by also matching `source_time`; the actual fix is a DuckDB sequence,
like `file_generations_id` already uses for file ids. Do that and the
`source_time` term in the predicate can go.

## 4. Validation never runs

`ViewSpec::validate` and `Scope::validate` are not called from
`DataService::open`, so a misconfigured view surfaces as a DuckDB binder
error inside the query pool rather than a config diagnostic (§10.1). Both
also reject derived dimensions as unknown columns, and neither takes
`DerivedDimensions` — so wiring them up requires threading that through
first. A derived name that shadows a real dataset column is accepted and
silently groups by the wrong one; that should be a load-time diagnostic.

## 5. Smaller, but real

- ~~`compile.rs` emits no `ORDER BY` when `view.sort` is empty~~ **DONE.**
  Depth now always leads the order, with the grouping columns following as
  tie-breakers so the order is total — otherwise two runs of one query can
  interleave a depth's rows differently and a tile that requeries on a
  timer reshuffles rows that did not change. Measured before the fix: the
  grand total came back at position 54 of 125.
- Derived view columns are unconditionally `Additive`/`Direct`; an
  expression over a `NonAttributable` or `SemiJoined` measure carries a
  wrong marker. Take the meet of the referenced columns' attributions.
- The `else 0` arm of the `sub_depth` CASE is unreachable but fails
  unsafely — it would attach the grand total. `else -1` fails loudly.
- The live ENUM cast has no value-safety guard; `try_cast` would make a
  stale ENUM degrade instead of erroring.
- `Scope::columns()` returns `[]` for a contradiction, so a UI rendering
  scope chips from it shows "no scope" for a scope that selects nothing.
- Derived-dimension grammar errors surface as `StoreError::Sql` rather
  than a `Diagnostic` at the point of entry.
- `DataService::freshness()` is unconditionally live (no caller yet, but
  it is the next place the era class lands).
- `Era::live()` is test-only and `Era` is not re-exported from
  `query/mod.rs` — a standing invitation to re-hardcode `TableKind::Live`.

## How to work on this

Read `docs/phase-2b-review-handoff.md` first — it records why five rounds
were needed and what the recurring defect classes are. Then:

- Run `zsh scripts/mutation-check.sh` before and after. All 27 entries
  currently pass; add one per behaviour you change.
- Verify by execution, not by reading. Every defect in Phase 2b was found
  by running SQL, and several tests that read as thorough turned out to
  pass with the feature deleted.
- The fixture gaps listed at the end of the handoff are the best
  available predictor of what the next round finds.

## Also outstanding, unrelated to the read path

Cold start (`docs/perf.md`): parallel staging measured at 1.87× at
realistic file sizes, plus a ~111ms per-file fixed cost that batching the
publish across grains would attack. Stated as a priority, deferred until
after 2b, and to be re-measured against a real network share rather than
local SSD. And `[sources]` still has no real config surface — the probe
reads `GEODE_PROBE_DIR` instead, which goes away with the probe.
