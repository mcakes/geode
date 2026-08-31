# Phase 2b review handoff

Written 2026-08-30, at branch `phase-2b-query-path` commit `2029e91`, for
whoever picks up the next review/fix round.

## Where this stands

Phase 2b (the query path) is feature-complete and has been through **four
review rounds**. Each round found Critical, execution-verified defects:

| Round | Found | Fixed in |
|---|---|---|
| 1 | 7 Criticals — semi-join scoping, NULL fan-out, join multiplication, NULL join keys, depth-0 joined views, as-of on joined datasets, derived dimensions never materialized | `a7a1cb0` |
| 2 | 2 Criticals — both *siblings* of round 1's, in a code path round 1's fix had unblocked | `8ac80e4` |
| 3 | 3 Criticals — param/clause desync, era-blind ENUM cast, NULL books dropped from history | `e70c2db` |
| 4 | 2 Criticals — generations resolved from one grain, `is_finer` wrong for same-grain measures | `2029e91` |

All four rounds are green on `cargo test --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo fmt --check`, and
`cargo bench --workspace --no-run`.

## The thing to understand before starting

**The recurring failure mode is not bad reasoning, it is an unreachable
fixture.** Every round fixed exactly what was pointed at, and the next
round found the same *class* somewhere adjacent. Round 4 diagnosed why:
of eight load-bearing behaviours it broke on purpose, **five left the
test suite completely green**. The suite asserted that code ran, not that
it was right.

Concretely, in round 4 one of the tests written *that same session*,
specifically to close a gap round 3 had named, turned out to pass with
the feature deleted — it correlated a semi-join on a key that never
matched (`lhu='GONE'` vs `'L0'`). Reading it did not reveal that. A
mutation did.

So: **do not trust a green suite here, and do not trust a test because it
looks thorough.** Break the code and see if anything notices.

## Use the mutation harness

`scripts/mutation-check.sh` — twelve mutations across the compiler, scope
lowering, and as-of routing. All twelve are currently caught. Run it
after any change to those files:

```sh
zsh scripts/mutation-check.sh
```

A `SURVIVED` line means a branch no test can see. **The harness is not
complete** — it covers what four rounds happened to touch. Adding
mutations for whatever you work on is the highest-value thing you can do
here, and probably higher-value than another read-through.

## Defect classes that have recurred (check these first)

Three classes have each bitten in more than one place. When you touch
anything, ask where else the class lives rather than fixing only the site
in front of you — that habit is what the first four rounds lacked.

1. **Era / as-of routing.** Every site that names a table or builds a
   WHERE clause must consult the caller's era (`Era { kind, generations }`
   in `query/scope_sql.rs`). Missed historically in: cross-dataset joins
   (r1), the semi-join probe (r2), the ENUM cast (r3), generation
   resolution grain (r4). Known remaining: `DataService::freshness()` is
   unconditionally live (no caller today).
2. **NULL-unsafe matching.** For every `=` / `is not distinct from` on
   keys, decide whether NULL there is a rolled-up placeholder (`=`) or a
   real value on both sides (`is not distinct from`). NULL books and NULL
   LHUs are ordinary — ingest reports them rather than dropping them.
   Known remaining: `store/publish.rs::partition_predicate` emits
   `book = '…'`, so a NULL-book partition is never replaced on republish
   and live accumulates duplicates (violates the §4.3 invariant its own
   module header claims).
3. **Derived dimensions (§6.8).** Every path consuming a column name can
   receive a derived one. Missed historically in: grouping + selections
   (r1), scope expressions (r2). Known remaining: `ViewSpec::validate`
   and `Scope::validate` both reject derived names as unknown columns; a
   derived name shadowing a real column silently groups by the wrong one.

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
deliberately blanks reads back as `0.0` and no accessor can tell
otherwise — add `f64_value(name, row) -> Option<f64>`. `dict_column`
hardcodes `UInt8` keys, so a dimension with more than 255 values falls
through both it and `str_column` and renders blank; DuckDB promotes to
`UInt16` above 255. `dict_value` ignores the null bitmap, so a NULL
dimension cell returns dictionary entry 0.

**Compiler — `query/compile.rs`:** no `ORDER BY` is emitted when
`view.sort` is empty, so "shallowest first, parent before children" does
not actually hold. The depth-0 spine full-scans the table for a row it
already knows. The `else 0` arm of the `sub_depth` CASE is unreachable but
fails unsafely if ever reached (`else -1` would fail loudly). Derived
view columns are unconditionally marked `Additive`/`Direct`.

**Validation:** `ViewSpec::validate` and `Scope::validate` are never
called from `DataService::open`, so a misconfigured view surfaces as a
DuckDB binder error inside the query pool rather than a config
diagnostic. A scope on a column finer than the spine grain compiles fine
and fails at execution. Derived-dimension grammar errors surface as
`StoreError::Sql` rather than a `Diagnostic` at point of entry (§10.1).

**Smaller:** the live ENUM cast has no value-safety guard (`try_cast`
would fix it); `generation_predicate` inlines one term per partition, so
statement text grows with partition count and defeats the plan caching
`scope_sql` works to preserve; `Scope::columns()` returns `[]` for a
contradiction, so a UI rendering chips from it shows "no scope" for a
scope selecting nothing; `Era::live()` is test-only and `Era` is not
re-exported.

## Fixture gaps (these predict the next round's findings)

`hostile_fixture()` in `query/compile.rs` tests has a NULL book, a NULL
LHU, two archived generations, an LHU present only in history, a
cash-only partition (position grain only), and ENUMs built from live.
Still **not** represented anywhere:

- two generations of one partition at the *same* `source_time` —
  `row_number() over (… order by source_time desc)` is then
  nondeterministic in both `as_of.rs` and `retention.rs`, and they can
  disagree
- more than 256 distinct values in a derived ENUM
- a live query against the hostile store (both hostile tests are as-of)
- a dimension value present at a finer grain but absent from the table
  its ENUM was built from
- a scope expression naming an attribute (measures are now covered)

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
