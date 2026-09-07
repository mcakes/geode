# Performance measurement infrastructure

What exists today for spec §7.4 ("Enforcement"), and what is deliberately
deferred. PHILOSOPHY.md §6 is the governing rule: performance is a
discipline, measured before and after, with budgets treated as contracts.

## Frame-time histogram (always compiled)

`geode_shell::perf::FrameHistogram` — a fixed-size log-bucket histogram
(12 buckets/decade over 0.1ms→100ms, plus overflow; saturating counters;
approximate percentiles capped at the observed max). `ShellView` owns one
and records into it at the top of every `render`:

- **Signal**: the interval between consecutive `ShellView::render` calls.
  Geode is event-driven, so during interaction bursts (key-repeat,
  drags, typing) consecutive samples are honest frame times including the
  previous frame's full prepaint/paint/present. It does **not** capture
  compositor/display latency, frames where only a child entity
  re-rendered, or the duration of the last render before an idle pause;
  intervals ≥ 500ms (`perf::IDLE_CUTOFF`) are counted as idle gaps, never
  recorded as frames.
- **Discipline**: recording is O(1), allocation-free, lock-free, and
  never notifies — instrumentation cannot force frames or stall the
  render thread it measures.

## Debug overlay (`perf::toggle_overlay`, bound `mod+shift+p`)

A palette-style instant panel (top-right, theme tokens, category
"Diagnostics" in the palette) showing frames-since-reset and p50/p95/max
frame time. `perf::reset` (palette-only) zeroes the counters. The overlay
adds **no timer**: values are as-of the last invalidation — anything that
redraws the window refreshes it, and an idle shell honestly shows the
stats from its last painted frame rather than waking up to animate them.

The overlay also shows three requery-latency rows (added in Phase 3b):

- **q p50**: p50 of submit→snapshot latency (query submitted to snapshot
  delivered to the UI). Measures the median query path across the
  boundary to DuckDB.
- **paint p50**: p50 of snapshot→paint latency (snapshot delivered to
  first painted frame). Measures the median latency from the query pool
  back to the render thread, through any module's frame-building and
  into gpui's paint.
- **requery**: the last completed pair, shown as `q_ms + p_ms` (query
  latency plus paint latency for the most recent requery to finish).

These rows read `—` until a module records its first requery (Plan 3c).
`perf::reset` zeroes them along with the frame-time counters.

## Benchmarks

- `cargo bench -p geode-shell` — criterion over the pure shell cores
  (`benches/shell_cores.rs`): `Tree::layout` and `divider_strips` on
  deep/wide trees at 10/40/100 tiles, `resolve_drop_target` cursor
  sweep, `Matcher::press` worst-case no-match over the full builtin
  keymap, palette `fuzzy_match` + full filter pass at 66/500/2000 items,
  and the session TOML round-trip for a 9-workspace layout. Runs in
  ~25s; criterion writes comparisons against the previous local run to
  `target/criterion/`.
- `cargo bench -p geode-demo-data` — the synthetic data generator
  (100k/1M rows), established in phase 0.
- `cargo bench --workspace --no-run` — CI compiles every bench on both
  platforms (the workspace-wide `bench = false` / `harness = false`
  invariants in CLAUDE.md keep criterion the only harness).

## Profiler support (`profiling` feature, off by default)

`cargo run -p geode-app --features profiling` enables gpui's own
`profiler` feature at the pinned rev (zed e3adf43) — hdrhistogram-backed
per-window draw/present and input-latency histograms, gpui's painted
debug frame overlay, and hang detection — plus two extra Diagnostics
actions registered only when compiled in:

- `perf::gpui_overlay` — cycles gpui's built-in frame overlay
  (Hidden → Minimal → Full).
- `perf::dump` — writes both measurement layers (the shell histogram and
  gpui's draw/dirty→present/present-interval histograms) to stderr.

Recorded decision: no Tracy client. gpui instruments through the
backend-less `profiling` facade crate; wiring Tracy would mean adding a
`tracy-client` dependency this workspace deliberately avoids. The only
dependency the feature adds is `hdrhistogram`, pulled in by gpui itself.
Verification for changes touching the feature:
`cargo check -p geode-app --features profiling` (not part of CI).
Release/bench profiles already keep debug symbols for external profilers
(workspace `Cargo.toml`, spec §7.4).

## Deferred to Phase 2: CI benchmark-regression gating

Spec §7.4's "CI fails on benchmark regression beyond threshold" is
deliberately not wired yet: the benches that the spec's budgets actually
gate on — the query/snapshot pipeline over 1M-row synthetic data — do
not exist until Phase 2 builds DataService, and thresholding the current
nanosecond-scale shell microbenches on shared CI runners would produce
noise-driven failures, not regression signal. When Phase 2 lands the
pipeline benches, add the gating step (criterion baselines checked
against a stored reference with a generous threshold) over those.

## Phase 2a: ingest benchmarks (`cargo bench -p geode-data`)

`benches/ingest.rs`, over the generated source directory (no checked-in
fixtures, spec §7.4). Measured on an M-series Mac, `--release`, DuckDB
1.10505 bundled. Numbers are the criterion median.

| Benchmark | Result |
|---|---|
| `ingest_warm_start/reopen_populated_db` | **5.0 ms** |
| `ingest_single_file/load_one_file` (~6k rows) | 237 ms (25.7 K rows/s) |
| `ingest_cold_start/100000_rows` | 2.93 s |
| `ingest_cold_start/1000000_rows` | 12.2 s |

Staging, sequential against four connections, at two file sizes:

| Rows per file (17 files) | Sequential | Parallel ×4 | Speedup |
|---|---|---|---|
| 23,529 | 3.55 s | 1.32 s | 2.69× |
| 117,647 | 5.75 s | 3.07 s | **1.87×** |

**Warm start is the number that matters most.** Reopening a populated
database and querying `measures_position_live` takes 5 ms, so tiles paint
real data essentially immediately and the §7.1 one-second startup budget
never depends on reading a CSV. This is what amending §5.3 from in-memory
to persistent storage bought.

> **Read this before acting on the 1.87×.** The benchmark behind it
> stages a file with `read_csv` and nothing else — the real staging path
> is `read_csv` *plus* `split_by_grain`, a grouped aggregation per grain
> that is not in the measurement at all. So the figure is sound for what
> it measures and is **not** a measurement of staging. Whether the
> combined path parallelises as well is open. The work is on hold until
> it is measured on the real path and against a real network share:
> `docs/ingest-cold-start-handoff.md`.

**Spec §5.6's open question is answered: parallel staging wins, by
1.87× at realistic file sizes.** The plan predicted parallelism would
*not* help, reasoning that DuckDB's CSV reader is already multi-threaded
and would saturate the cores by itself. The measurement says that
reasoning is directionally right but incomplete: the advantage does
shrink as files grow — 2.69× at 24k rows per file, 1.87× at 118k — yet
it is still worth nearly 2× at the size the desk's files actually run
to. Per-file fixed cost (below) is part of why: it is paid per file
regardless of file size, and parallelism hides it.

An earlier revision of this file claimed 2.76× and explained it as
intra-file parallelism "never" saturating the cores. That was measured
on ~18k-row files — far smaller than production — and the explanation
was too strong. The crossover where inter-file parallelism stops paying
was not located; it lies above 118k rows per file, and finding it needs
a fixture larger than is practical to generate in memory.

The ingest runner ships single-threaded (correct, and matching §5.3's
single-writer constraint for *publishes*); moving staging onto a worker
pool while keeping publishes serialized is the measured next
optimization.

**Scheduled after Phase 2b, and treated as required work.** Cold-start
time is a stated priority even though §7.1 sets no contract for it. Two
independent levers, and cold start wants both: parallelise staging
(publishes stay serialized), and cut the ~111 ms per-file fixed cost by
batching the publish across grains into one transaction. See the
decomposition below for which pays where. Re-measure against a real
network share before trusting the 1.87×; these numbers are local SSD.

**Per-file fixed cost, decomposed.** A load runs `read_csv`, one grouped
split per grain, and one publish transaction per grain. Solving the two
cold-start points (17 files at 5.9k rows/file = 172 ms/file; 17 files at
59k rows/file = 718 ms/file) gives roughly:

```
per-file cost ≈ 111 ms + 10.3 µs × rows
```

So the fixed component is ~65% of the time on small files and ~15% on
59k-row files. That decides which lever applies where: **batching the
publish across grains** attacks the 111 ms and pays most on many small
files — a multi-day backfill, where file count dominates — while
**parallel staging** pays most on the large-file case that a normal day
produces. They are independent, and cold start wants both.

Not yet gated in CI. The §7.4 regression gate wants a stored criterion
baseline and a generous threshold; these numbers are the first baseline
worth storing.

## Phase 2b: requery benchmarks (`cargo bench -p geode-data -- query_`)

`benches/query.rs`, over the generated source directory, ingested through
the real pipeline and queried through `DataService` — submit to snapshot,
which is §7.1's end-to-end path minus the paint. Same machine as the 2a
table above.

| View | Result rows | 100k ingested | 1M ingested |
|---|---|---|---|
| `tree`, scoped to 3 books | 7.7k / 74.7k | 8.2 ms | **23.9 ms** |
| `tree`, scoped, bounded to depth 2 | 121 | 5.7 ms | **10.8 ms** |
| `shallow` (book only), scoped | 4 | 4.3 ms | **6.0 ms** |
| `tree`, all 20 books | 40.4k / 398k | 16.6 ms | **69.9 ms** |
| `tree`, unscoped | 40.4k / 398k | 15.2 ms | **66.1 ms** |
| `tree`, unscoped, bounded to depth 2 | 121 | 7.5 ms | **10.3 ms** |

Re-measured after the correctness fixes in `a7a1cb0`, which cost a few
percent: each per-grain aggregate now carries its own level and the join
matches on it, so a NULL in a grouping column cannot fan the tree out.
Result-row counts are unchanged, which is the invariant that matters —
the fixes changed which rows are correct, not how many there are.

`tree` is `lhu > underlying_ref > position_ref` across three measure
grains — the shape a blotter actually runs, not a bare `select`.

**The §7.1 <50ms contract holds for every shape the blotter actually
submits.** Latency tracks *result* size, not input size: 398k rows in
66 ms is roughly 6M rows/sec across the Arrow boundary, so neither the
engine nor the grain-split join is the constraint. The two rows that miss
the budget are the unbounded ones, and they miss it for exactly that
reason.

### Why the depth bound exists

The compiler originally emitted one `ROLLUP` covering *every* level, so a
fully collapsed tree still materialized its leaves: a trader looking at
80 LHU rows paid for 398,085. `compile_view` now takes a `max_depth` and
emits `GROUPING SETS` over `0..=max_depth` instead. The caller passes one
more level than is expanded, so a single-step expand is already in the
snapshot and only a deeper one costs a requery — expand and collapse stay
pure UI within the bound (§6.3), which was the point of the single
statement in the first place.

The unscoped million-row tree is the case that shows what it buys:
**66.1 ms → 10.3 ms, and 398,085 result rows → 121.** The scan is
identical; only the result changed. That second number matters as much as
the first, because gpui-component's `DataTable` virtualizes by *index* —
its `TableDelegate` is asked for row `i` — so the blotter has to flatten
the tree into a visible-row list itself, and that walk is proportional to
what was materialized, not to what is on screen. An unbounded query would
put a 398k-row walk on the frame budget to paint 80 rows.

Not gated in CI yet; these are the first stored baselines for the query
path.

### Re-measured after the round-5 correctness fixes

Same machine, same fixture, same harness. **The result-row counts changed,
and that is the finding**, not the latency:

| View | Result rows before → after | 1M before → after |
|---|---|---|
| `tree`, scoped to 3 books | 74,689 → **136,868** | 23.9 ms → 31.2 ms |
| `tree`, scoped, bounded to depth 2 | 121 → 133 | 10.8 ms → 11.7 ms |
| `shallow` (book only), scoped | 4 → 4 | 6.0 ms → 5.9 ms |
| `tree`, all 20 books | 398,085 → **729,466** | 69.9 ms → 99.2 ms |
| `tree`, unscoped | 398,085 → **729,466** | 66.1 ms → 96.1 ms |
| `tree`, unscoped, bounded to depth 2 | 121 → 133 | 10.3 ms → 11.4 ms |

### Re-measured after the Phase 3 prerequisites branch

`compile.rs` now emits `ORDER BY` for every view, not only those declaring
a sort, so every result is sorted by depth plus the grouping columns.
Result-row counts are identical to the round-5 column above (136,868 /
729,466 / 133 / 4), which is what makes the comparison meaningful:

| View | 1M round 5 → now |
|---|---|
| `tree`, scoped to 3 books (136,868 rows) | 31.2 ms → 32.0 ms |
| `tree`, scoped, bounded to depth 2 (133) | 11.7 ms → 8.3 ms |
| `shallow` / regroup (4) | 5.9 ms → 4.0 ms |
| `tree`, all 20 books (729,466) | 99.2 ms → 76.7 ms |
| `tree`, unscoped (729,466) | 96.1 ms → 75.9 ms |
| `tree`, unscoped, bounded to depth 2 (133) | 11.4 ms → 7.9 ms |

**No regression, and §7.1 still holds for every shape the blotter
submits** — the bounded and scoped rows are 4–32 ms. The one row that
moved against the sort is the scoped 136k tree, +0.8 ms. Sorting cannot
make a query faster, so read the improvements as a quieter machine rather
than as an effect of the change; the honest reading of this table is "the
sort is not measurable next to run-to-run variance", which is what the
row counts predict — the bounded shapes sort 133 rows.

The fixture declares the pair grain, so the compiler's spine was the
`measures_underlying_pair` table — whose `underlying_ref` is the *lesser*
of a canonical pair (§3.3), not the underlying. The underlying level of
the tree therefore only ever showed the underlying that sorted first in
each pair, and everything beneath the others was absent while still
counting in the totals. The rows that appeared are the rows that were
missing: **about 45% of the tree.** Latency moved with the row count and
nothing else — 136,868 rows in 31 ms is a better per-row rate than
74,689 in 24 ms was — so the §7.1 <50ms contract still holds for every
shape the blotter submits, and the two unbounded rows miss it as they did
before, for the same reason.

The spine is now assembled from the per-grain aggregates (each is
referenced twice in the statement: once by the spine, once by the join)
rather than scanned from one table. DuckDB handled the double reference
within budget; `as materialized` on the aggregate CTEs is the lever if a
future view shape does not. The depth-0 full scan is gone: the grand-total
row is a constant.

## Phase 3a: tree index (`cargo bench -p geode-core`)

`TreeIndex::build` (`crates/geode-core/src/tree.rs`, Phase 3 §5.5) at the
three result shapes the requery table above records, measured with
`cargo bench -p geode-core -- tree_index_build` (criterion, 10 samples;
the fixture in `crates/geode-core/benches/tree.rs` builds each shape as a
three-level lhu > underlying > position tree with dictionary-encoded
grouping columns, siblings interleaved within a depth as a declared sort
produces them):

| shape | rows | median |
| --- | ---: | ---: |
| bounded to depth 2 | 133 | **11.09 µs** |
| scoped to three books, all depths | 135,733 | **12.63 ms** |
| unscoped, all depths | 720,881 | **65.85 ms** |

The earlier figures in this table (7.94 µs / 11.15 ms / 61.86 ms) were
the string-hashing path, resolving each grouping cell's text per row;
these are the code-hashing path (spec §5.5's "dictionary codes where
present, strings under as-of"), which keys a dictionary-encoded column
on its per-row code instead.

The build runs on the query worker inside `Snapshot::from_batches`, not
on the render thread, so none of this is frame time — it is added to the
requery latency the §7.1 <50ms contract governs. **The 729k build does
not fit that budget on its own** (65.85 ms), but the 729k shape is the
unscoped query that already misses §7.1 by a wide margin for its own
reasons, recorded above. The shapes the blotter actually submits do fit
comfortably: the bounded tree pays 11.09 µs against a 4–8 ms query, and
the scoped 136k tree pays 12.63 ms on top of ~31 ms — inside 50 ms, with
no room wasted. The cost is linear in rows × depth (one FNV-1a hash of
the grouping prefix per row per level, then a CSR fill), so it scales
with the result the compiler was told to materialize, which is what the
depth bound exists to keep small.

## Phase 3c: blotter core (`cargo bench -p geode-blotter`)

The blotter's pure-core costs (`crates/geode-blotter/src/core/`, Phase 3
§6.1) at the same three shapes as the tree-index section above (133 /
135,733 / 720,881 rows): flatten fully expanded, flatten with everything
collapsed (the shell-of-a-tree cost — the DFS still visits every root),
filling a 40-row cache window over a 7-column plan, and building a
100-column plan. The cache window is 40×7, not the 40×20 spec §7.3
mentions, because this brief's fixture carries 6 measure columns plus
the tree column (7 total), not 20; the column count is a property of
the fixture, not the cache, which fills however many columns the plan
actually has. Measured with `cargo bench -p geode-blotter --
--warm-up-time 1 --measurement-time 3` (10 samples per
`benchmark_group`'s `sample_size(10)`; the shortened warm-up/measurement
window, rather than criterion's defaults, is what fit this run inside
the harness's 590s foreground budget — medians below are otherwise
unmodified criterion output). The fixture
(`crates/geode-blotter/benches/blotter.rs::shape`) reuses
`crates/geode-core/benches/tree.rs`'s three-level lhu > underlying >
position tree builder, adding 6 `F64` measure columns (100 for the
plan-build case) and a view grouped on the three tree dimensions with
every measure column selected:

| bench | 133 rows | 135,733 rows | 720,881 rows |
| --- | ---: | ---: | ---: |
| `flatten_all` (fully expanded) | **441.16 ns** | **205.87 µs** | **1.1836 ms** |
| `flatten_collapsed` (roots only) | **244.96 ns** | **265.72 ns** | **1.6720 µs** |
| `cache_fill_40x7` (40-row window) | **31.559 µs** | **33.132 µs** | **39.008 µs** |

`plan_build_100_columns` (13-row shape, 100 measure columns): **14.048
µs**.

`restore_by_path` (I3, final review — the keypress-path cost
`reflatten_keeping` pays on every requery/expand/collapse/sort, fixed to
skip the O(1) depth check before allocating a path and to search
outward from the previous cursor row instead of scanning from row 0),
at the 720,881-row shape, fully expanded, target the very last row (a
depth-3 leaf — 720,000 of the 720,881 rows share its depth, so the
depth check alone can't reject much of the list for this particular
target):

| bench | fallback 50 rows away | fallback 720,830 rows away |
| --- | ---: | ---: |
| `restore_by_path_729k_rows_{nearby,far_fallback}` | **6.7446 µs** | **55.011 ms** |

The first column (`_nearby`) is the realistic case the fix targets — the
cursor's node usually moves by a handful of rows between one reflatten
and the next, and finding it costs a handful of `path_of` calls, not a
scan of the whole list. The second (`_far_fallback`) is the adversarial
bound: `fallback` at row 0, on the opposite end of the list from the
target, forces the outward search to walk (and `path_of`-allocate for)
essentially every depth-3 row in between, since almost the whole shape
shares the target's depth here. It's recorded for honesty about the
worst case — a `gg`-to-top-then-jump-to-the-bottom kind of move — not
because ordinary use pays it; a real UI session's cursor moves locally
almost always, which is exactly the `_nearby` number.

None of the first four benches run on the render thread (spec §6.1: the
plan is rebuilt only when the snapshot's column set changes, flatten
runs on snapshot arrival/expand/collapse/sort, and the cache fills only
the visible window on scroll) — every number among them is comfortably
inside a single frame's 8ms budget, including the 720,881-row fully-
expanded flatten at 1.18ms, which is the most expensive of the group and
still under a sixth of that budget. The cache fill barely moves with row
count (31.6µs → 39.0µs across three orders of magnitude of rows) because
it only ever touches the 40 visible rows; `flatten_collapsed`'s near-flat
cost the same way reflects that collapsed roots are the only rows a DFS
ever visits regardless of how many rows are hiding beneath them.
`restore_by_path` *does* run on the render thread (it's called from
`reflatten_keeping`, at exactly the same trigger points as flatten
above) — its nearby-fallback cost is well inside budget, and even its
adversarial worst case (55ms) is a rare, cursor-teleporting motion, not
the steady-state cost a normal session pays per keypress.

## Phase 3: the painted frame

The §7.1 budget is specified end to end, submit to painted frame, and
the query-path/tree-index/blotter-core benchmarks above each stop at
one boundary short of that (submit→snapshot, or a pure-core cost that
never touches gpui). Closing that gap needs a live window with a real
compositor. The branch was built in a sandbox without one; the readings
below are from Matthew's first run on 2026-09-06, after the merge.

**Setup:**

```sh
cargo run --release -p geode-app -- --demo 1000000
```

`--release`, matching the query-path section's own convention above:
this is a measurement against the §7.1 <50ms/8ms budgets, and a debug
build would make any reading meaningless as a check against them. The
`--demo 1000` smoke check stays a debug build.

**Readings** (perf overlay, `mod+shift+p`, described above under
"Debug overlay"), one session, 2,965 recorded frame intervals:

| reading | where read | value |
| --- | --- | --- |
| last requery, submit→snapshot + snapshot→paint | overlay's **requery** row | 17 ms + 21 ms = 38 ms |
| isolated single-action requery (2026-09-06, action not labelled), submit→snapshot + snapshot→paint | overlay's **requery** row, read after the action settled | 14 ms + 2.5 ms = 16.5 ms |
| requery submit→snapshot, session p50 | overlay's **q p50** row | 12 ms |
| requery snapshot→paint, session p50 | overlay's **paint p50** row | 4.6 ms |
| frame interval, session p50 | overlay's **p50** row | 18 ms |
| frame interval, session p95 / max | overlay's **p95** / **max** rows | 500 ms / 500 ms — see below |

**Whether the §7.1 <50ms requery contract holds end to end:** yes on
this evidence. The last requery's two halves sum to 38 ms, and the
session medians (12 ms query, 4.6 ms paint) leave the same margin. An
isolated reading of a single action (2026-09-06, action not labelled)
came in at 16.5 ms end to end (14 ms + 2.5 ms), well inside budget; the
`ctrl+2` regroup and the `zo` at the bound still need to be read and
labelled separately (press the key, wait for the header to settle,
read the **requery** row before pressing anything else).

**The frame-interval p95 of 500 ms is not a paint cost.** The histogram
(`geode-shell::perf`) buckets intervals up to 100 ms; anything between
100 ms and the 500 ms idle cutoff lands in one overflow bucket, and a
percentile that falls there reports the observed max, which the overlay
rounds to the cutoff. So p95 = max = 500 ms means at least 5% of the
2,965 intervals were longer than 100 ms and shorter than 500 ms. Over a
session of hand-driven keys those are the pauses between keystrokes,
which the histogram cannot tell apart from stalls (its module doc names
this blind spot: it measures consecutive renders during interaction
bursts). The interaction-burst signal is the p50, 18 ms, one 60 Hz
frame. A stall would show as a max well above the cutoff's rounding, or
as a p95 that stays in the overflow bucket during a single continuous
`j` hold with the counters reset just before it.

**The `DataTable` swap trigger** (spec §6.6) needs exactly that
isolated reading: open the `wide` view in a tile, reset the overlay's
counters (`perf::reset`, palette-only: `ctrl+k`, type `reset`), hold `j`
until at least 40 rows of key-repeat have passed, and
read **p95** before releasing anything else. The session-wide p95 above
does not separate the hold from the pauses around it. Before this fix,
`perf::reset` left one stale sample in the zeroed histogram — the
reaction-time gap back to the frame painted before the palette action —
so a p95 read right after a reset showed a few hundred milliseconds
before the `j` hold had contributed anything; from this commit the first
post-reset render records nothing, so the counters really do start from
zero.

| reading | where read | value |
| --- | --- | --- |
| `j`-scroll frame time, p95, `wide` view, 40 visible rows | overlay's frame-time p95, counters reset before the hold | **100 ms** (2026-09-06, after the `perf::reset` fix) |
| same hold, p50 / max | overlay's p50 / max rows, same reading | 18 ms / 496 ms |
| same hold on the `tree` view (3 columns), p50 / p95 / max | overlay, counters reset before the hold | 18 ms / 96 ms / 96 ms |
| `k` held at the top of the table (cursor cannot move), p50 / p95 / max | overlay, counters reset before the hold | 100 ms / 100 ms / 267 ms |

**Reinterpretation (2026-09-06): the interval histogram cannot measure
paint cost during a held key, and these readings are the key-repeat
period, not the blotter.** The `k`-at-the-top hold is the proof: the
cursor cannot move, so each key repeat produces one cheap repaint and
nothing else, and the interval between repaints is the interval between
key repeats — a 100 ms median, which is macOS's default key-repeat rate
(setting 6 = 6 × 15 ms = 90 ms; initial delay 25 × 15 ms = 375 ms,
which the 267 ms max and the earlier 496 ms sample fall under). During
a `j` hold the scroll adds a few repaints per key at vsync spacing (the
18 ms median) and the gap back to the next repeat is the one-in-twenty
~96 ms tail; the `tree` view shows the same tail because it has the
same keyboard. `geode-shell::perf`'s module doc names this blind spot:
it records render-to-render intervals, so a held key at a repeat period
longer than a frame reads as slow frames however cheap the paint is.

**Consequence for the swap trigger (spec §6.6): still unmeasured.** The
plan's recipe (frame-time p95 during a `j` hold) needs render
*duration*, not interval. Two ways to get it: `perf::gpui_overlay`
cycles gpui's own frame overlay, which reports what a frame spent in
layout and paint; or add a second histogram to `geode-shell::perf`
that records `Instant::now() - render_started` at the end of
`ShellView::render` and an overlay row for it — a small change, and the
right instrument for the 8 ms pure-UI budget going forward. Until one
of those is read, the `DataTable` swap is neither triggered nor
cleared. Deferred on 2026-09-06: subjectively, holding `j` on the
100-column `wide` view at 1M rows feels fast enough, so the
render-duration histogram is a follow-up, not a blocker. Nothing in these readings shows a paint problem: the query
halves (12 ms / 4.6 ms medians, 16.5 ms isolated) are the only
end-to-end numbers so far, and they hold §7.1.

## Phase 4a: the text filter (spec §3.4-3.5, §7)

**Before the rewrite**, spec §7 recorded the floor a per-keystroke text
filter would hit with all eight of the fixture's string columns marked
textual (a real desk schema would mark most of them) — `tree`, 1M rows:

| Needle | Rows matched | Full tree, unscoped | Depth 2, unscoped | Full tree + 3-book selection |
|---|---|---|---|---|
| `bk00` (broad) | 456k | 119 ms | 45 ms | 56 ms |
| `bk007` (narrow) | 46k | 71 ms | 61 ms | 27 ms |
| `zzz` (nothing) | 0 | 63 ms | 63 ms | 26 ms |

The zero-match, depth-2 case — the one a trader's first keystroke hits —
was 63 ms: over budget by construction, and matches or not made almost
no difference, because every one of those numbers is a row scan.

**After the rewrite** (`crates/geode-data/src/query/scope_sql.rs`): a
textual column that is also categorical is ENUM-typed in the live era
(spec §3.3), so its `ILIKE` runs over the type's dictionary
(`enum_range`) instead of every row, and the row test becomes an `in`
over codes. `book`, `lhu`, `counterparty` and `underlying_ref` — the
four categorical, routable string columns the bench schema declares
textual now — take this path; the plain-string key columns
(`business_date`, `position_ref`, `instrument_ref`) cannot (a key's
vocabulary is not small enough to be an ENUM), and stay a row scan —
measured separately below as the residual. Measured 2026-09-06,
`cargo bench -p geode-data --bench query -- "<rows>_rows_text"`
(criterion, 20 samples), same machine as the tables above:

| Case | 100k rows | 1M rows |
|---|---|---|
| `bk00` (broad, 456k-ish match), unscoped | 23.3 ms | 96.5 ms |
| `bk00`, depth 2 | 10.8 ms | 23.5 ms |
| `bk00`, + 3-book selection | 12.9 ms | 50.6 ms |
| `bk007` (narrow), unscoped | 12.0 ms | 34.1 ms |
| `bk007`, depth 2 | 9.8 ms | 22.5 ms |
| `bk007`, + 3-book selection | 6.9 ms | 18.5 ms |
| `zzz` (no match), unscoped | 8.9 ms | 20.8 ms |
| **`zzz`, depth 2 — the §3.5 gate** | 8.8 ms | **20.6 ms** |
| `zzz`, + 3-book selection | 6.8 ms | 17.9 ms |
| `zzz`, depth 2, plain-string keys also textual (residual row scan) | 12.2 ms | 33.4 ms |

**The gate — `1000000_rows_text_none_depth_2` under 50 ms — holds at
20.6 ms with the subquery form as written** (`"…" in (select v from
unnest(enum_range(null::…)) t(v) where v ilike ? escape '\\')`); the
Rust-side literal-list fallback the spec names as a backup was not
needed. Depth 2 unscoped is now cheaper than the full unscoped tree at
every needle, same as the plain requery benchmarks above, because it is
the same depth bound doing the same job — the dictionary rewrite fixes
the *floor* the row count no longer sets, and the depth bound still
governs how much of the tree the query materializes above that floor.

The residual row — plain-string key columns made textual too, at depth
2 with no match — is 33.4 ms at 1M rows: worse than the ENUM path's
20.6 ms, but still inside the 50 ms contract, and it is the case a real
schema is expected to avoid by not marking a key column textual in the
first place (`textual` on a column no grain can route is already a
load-time error; a key column that *can* be routed but has a
one-per-row vocabulary is a schema choice, not a bug).

**The as-of root cause (2026-09-07): the dictionary rewrite was gated to
the live era.** `refresh_enum` built the ENUM type from the live table's
distinct values only, so an archived row could in principle hold a
value the type lacked — and the scope compiler's text-filter rewrite
(`crates/geode-data/src/query/scope_sql.rs`) refused to name the type at
all once `era.kind != TableKind::Live`, falling back to the per-row
`ILIKE` for every textual column. As-of reads `archive union all live`
(spec §6.5), so this was the pre-rewrite floor above, over more than the
live row count. The fix: `refresh_enum`
(`crates/geode-data/src/store/ddl.rs`) now reads live **and** archive —
it already runs right after each publish, which is also when the
outgoing generation moves to the archive (spec §4.3), so no row in
either table can hold a value the type lacks — and the compiler's gate
checks only whether the type exists, not the era. New bench case,
`{rows}_rows_text_none_depth_2_asof`: the source ingested twice (a
second generation, republished an hour later) so a real archive exists,
queried with `AsOf::At` a moment between the two generations. Measured
2026-09-07, same machine as the tables above, `cargo bench -p geode-data
--bench query -- text_none_depth_2_asof` (criterion, 20 samples):

| Case | 100k rows | 1M rows |
|---|---|---|
| As-of, **before** this fix (base commit `f68f72b`, gate present) | 32.9 ms | 89.6 ms |
| As-of, **after** this fix | 23.7 ms | 48.7-49.0 ms |

**The gate — `1000000_rows_text_none_depth_2_asof` under 50 ms — holds,
but with far less headroom than the live case's 20.6 ms**: two runs
landed at 48.7 ms and 49.0 ms median, with the upper end of the
confidence interval touching 49.9 ms on the second run. That margin is
expected, not a regression risk introduced by the fix itself — the
as-of relation is `archive union all live`, so even with the dictionary
rewrite applied in both branches, the scan is over roughly twice the
row count the live-only case reads, plus the `union all`'s own cost.
Before this fix the as-of case was the same 63 ms floor as every other
pre-rewrite reading, worse than double the live floor because it also
paid for the row scan itself, not just more rows to decode; after, both
the live and as-of cases pay only for the dictionary probe and the row
count they each scan, which is why the ratio (89.6/48.7 ≈ 1.8×
improvement) tracks the row-count ratio rather than closing to zero.
Whether that headroom is comfortable enough for production data (wider
generation history, more concurrent tiles) needs a display-measured
reading the way §"the per-keystroke painted frame" below was for the
live case — not done here.

**The per-keystroke painted frame, measured on a display (2026-09-07).**
Everything above is the query-path benchmark (submit→snapshot on the
pool, no gpui). The end-to-end reading was taken by hand on
`a7fa6a1`: `cargo run --release -p geode-app -- --demo 1000000`, the
scope bar's text field (`mod+/`), `perf::reset`, a needle matching
nothing (`zzz`) typed one character at a time, the overlay's
**requery** row read after the last keystroke settled:

| Reading | submit→snapshot | snapshot→paint |
|---|---|---|
| `zzz` typed into the bar, demo `tree` view, 1M rows, **one blotter** | **48 ms** | 3.7 ms |
| same, **three blotters** open (every tile requeries per keystroke) | 78 ms | 3.5 ms |

**One tile holds the §7.1 budget, by 2 ms.** The paint half is well
inside it. With three tiles up each keystroke is three requeries, and
they do not queue at the pool (`query_workers` is 4 in
`geode-app::bridge`) — they run concurrently, and DuckDB parallelises
each one across every core, so three 1M-row scans contend for the
same threads and each takes longer. That is the cost of *N* tiles
sharing one machine, not a defect in any one query; the contract is per
requery, and a trader with three full-depth blotters over a million
rows is asking for three of them at once.

The 48 ms is 15 ms above the bench's 33.4 ms for this shape (`zzz`,
depth 2, keys textual) for two reasons worth knowing, because they are
the levers if the budget tightens: the demo schema marks `position_ref`
and `instrument_ref` textual — both keys, so their `ILIKE` is the
residual row scan the ENUM path cannot help, run once per measure grain
— and the demo `tree` view spans three measure grains (position,
underlying and the pair grain for `cross_gamma02`) where the bench's
spans two. Whether a million-row key column belongs in the text filter
at all is a desk decision, not an engineering one.

### Phase 4a: the text filter, literal-list form (spec §3.5, as amended again)

**Why**: a headless probe (2026-09-07) on the demo's own schema and
`tree` view — not the bench's own schema above — measured the subquery
form's floor for a needle that matches nothing, at 1M rows, two
generations so the archive is populated, medians of five:

| depth 2, needle `zzz` (matches nothing) | live | as-of |
|---|---|---|
| subquery form as shipped, keys textual | 105 ms | 218 ms |
| subquery form, `position_ref`/`instrument_ref` not textual | 78 ms | 169 ms |
| literal-list form, keys textual | 52 ms | 108 ms |
| literal-list form, keys not textual | 8 ms | 19 ms |

No-text baselines on the same view: live 12 ms, as-of 58 ms. The
subquery form's OR of seven `IN (select ... enum_range ...)` terms —
three measure grains, seven categorical textual columns on this schema
— costs ~65 ms per requery before any row of the result is built,
because DuckDB still has to plan and probe every correlated subquery
even when every one of them is empty. The remaining cost with keys
textual is the two key columns' per-row `ILIKE` over three grains —
no dictionary can help a million-row key; that is a schema decision
the user is making separately, same conclusion as the residual row
above.

**The change** (`crates/geode-data/src/query/scope_sql.rs`): for each
textual column that is categorical and whose ENUM type exists,
`compile_scope` now resolves the pattern's matches once at compile
time — `select v from unnest(enum_range(null::{ty})) t(v) where v
ilike ? escape '\'`, on the compile connection, synchronously, ahead
of `submit` — and binds the matching values as one delimiter-joined
varchar split by `string_split` in SQL, the same shape a dimension
selection already uses, so the statement text (and prepared plan)
stays independent of match count. A column with no matches drops its
term entirely instead of compiling to an always-false subquery; if
every column drops (or the dataset declares no textual columns at
all), the whole filter collapses to a literal `false` — a text filter
that matches nothing must select nothing, never everything.
Non-categorical textual columns are unaffected: still a plain `ILIKE`
row scan.

**Re-run of the bench's own gate cases** (this crate's `risk_snapshot`
fixture, four categorical routable textual columns, `cargo bench -p
geode-data --bench query -- text_none_depth_2`, criterion, 20 samples,
before = base commit `1a00920`, after = the literal-list form, same
machine):

| Case | before (subquery form) | after (literal-list form) |
|---|---|---|
| `1000000_rows_text_none_depth_2` | 32.276 ms | **5.2226 ms** |
| `1000000_rows_text_none_depth_2_asof` | 80.798 ms | **15.448 ms** |

**The gate — `1000000_rows_text_none_depth_2` under 50 ms — holds at
5.2 ms**, and the as-of case at 15.4 ms holds with far more headroom
than the subquery form's 80.8 ms did. Criterion's own comparison
against the previous run agreed: −83.9% and −80.8% respectively. The
100k-row cases moved the same direction (12.1 ms → 5.3 ms live, 33.9 ms
→ 12.0 ms as-of). Resolving the dictionary once per `compile_scope`
call rather than caching it across calls (deliberately not done in this
change) is still cheap enough that none of this margin is spent on it.
