# Performance measurement log

> **Archive:** This file preserves chronological measurements, fixtures, and
> investigations. See [`docs/current/performance.md`](current/performance.md)
> for maintained budgets, current reference values, and known gaps.

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
  `target/criterion/`. The matcher became an optimal alignment on
  2026-09-12 (greedy-leftmost painted `T[i]li[ng]` for `ling`), which
  costs about 3× per match — `fuzzy_match_one` 140 ns → 420 ns, the
  2000-item filter pass 180 µs → 360 µs, the 66-item pass (the real
  palette's size) 5.9 µs → 11.5 µs — all per keystroke, not per frame,
  and well inside the 8 ms pure-UI budget. The two `n × m` tables are
  allocated per call; a reusable scratch buffer is the first thing to
  reach for if a much larger list ever makes this visible. Later the
  same day the category joined the match text (`"{title} {category}"`,
  discounted past the title) and a per-row usage bonus joined the sort
  key: the per-item text is ~6–10 chars longer, so the 66-item pass
  read 13.2 µs (was 11.5), the 2000-item pass 405 µs (was 360), and
  `fuzzy_match_one` (a bare title, no category) 400 ns, unchanged. The
  bonus itself costs nothing per keystroke — one `u32` add per matched
  row, baked per row once at palette open. On 2026-09-26 multi-word
  queries began matching word by word in any order (per-word passes, a
  collision fallback, then the whole-query pass). Both bench queries
  (`tgl splt`, `spl wk`) take that path. Same machine and build, main's
  matcher then the new one: `fuzzy_match_one` 552 ns → 1.84 µs (a
  matching candidate pays every pass), the 66-item pass 19.0 → 24.5 µs,
  500 items 190 → 173 µs (noise, p = 0.32), 2000 items 615 → 696 µs.
  Rows missing a word stop after the per-word passes, which keeps the
  filter-pass cost small. The machine was loaded: main's own figures read
  above the earlier entries.
- `cargo bench -p geode-demo-data` — the synthetic data generator
  (100k/1M rows), established in phase 0.
- `cargo bench -p geode-documents` — the CVI document kind's parse and
  write cost at two grid shapes (`benches/cvi.rs`); see "Market-data
  documents" below.
- `cargo bench -p geode-data --bench publish_document` — the store-side
  publish cost at the same two shapes; see "Market-data documents"
  below.
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

#### Dictionary resolves once per statement

`compile_scope` ran `existing_enum_types` once per call and
`dictionary_matches` once per categorical textual column per call, and
`compile_view` calls `compile_scope` once per measure grain plus once
for the spine (when the spine fallback scan is reached at all — see
below) plus its own `existing_enum_types` for the interned-columns
check: several tiny catalog round trips repeating the same answer on
every keystroke. `DictionaryCache` (`crates/geode-data/src/query/
scope_sql.rs`) resolves each fact once per statement instead —
`compile_scope`'s own signature is unchanged (a thin wrapper over the
new `compile_scope_cached`, cache-free callers pay for exactly one
miss); `compile_view` and `compile_distinct` hold one cache across
their internal calls.

**This bench's schema is a smaller version of the demo's saving, not a
copy of it.** It declares four categorical textual columns (`book`,
`lhu`, `counterparty`, `underlying_ref`), and the `tree` view here
spans three measure grains (Underlying, Instrument, Position) — not
the two originally assumed going into this change. The spine fallback
scan is **not** a call site for this view: verified by hand (a
temporary print of `missing` in `compile_view_with_cache`, run through
the actual bench schema at every depth this suite exercises), the bench
`tree` view's grouping — `["lhu", "underlying_ref", "position_ref"]`
(`benches/query.rs:128`) — comes back fully carried at depths 1, 2 and
3, so `missing` is empty every time and `compile_scope_cached`'s spine
branch never runs. Call sites are therefore exactly the three measure
grains plus the interned-columns check: 3 × (1 + 4) + 1 = 16 catalog
queries without the cache, 1 + 4 = 5 with it. Smaller than the demo
view's 33 → 8 (Phase 4a's design doc: 4 × (1 + 7) + 1 = 33, cached to
1 + 7 = 8) because this schema carries fewer categorical columns and
one fewer call site — the same shape at smaller scale, not a
like-for-like reproduction of that figure.

1M rows, `cargo bench -p geode-data --bench query -- 1000000_rows_text`,
criterion, 20 samples, before = base commit `07d5051`, after = this
change, same machine:

| Case | before | after |
|---|---|---|
| `1000000_rows_text_broad_unscoped` | 90.099 ms | 89.160 ms (noise) |
| `1000000_rows_text_broad_depth_2` | 11.003 ms | **9.9469 ms** (−9.5%) |
| `1000000_rows_text_broad_with_books` | 46.633 ms | **45.687 ms** (−2.0%) |
| `1000000_rows_text_narrow_unscoped` | 22.621 ms | **21.597 ms** (−4.9%) |
| `1000000_rows_text_narrow_depth_2` | 7.9846 ms | **6.9121 ms** (−13.4%) |
| `1000000_rows_text_narrow_with_books` | 7.6853 ms | **6.6369 ms** (−13.8%) |
| `1000000_rows_text_none_unscoped` | 3.6895 ms | **2.5877 ms** (−29.9%) |
| `1000000_rows_text_none_depth_2` | 3.6285 ms | **2.5109 ms** (−30.8%) |
| `1000000_rows_text_none_with_books` | 3.8944 ms | **2.8188 ms** (−27.6%) |
| `1000000_rows_text_none_keys_textual_depth_2` | 25.198 ms | 25.498 ms (noise) |
| `1000000_rows_text_none_depth_2_asof` | 8.5486 ms | **7.8563 ms** (−8.1%) |

Nothing regressed (criterion's own before/after comparison called
every row above either "improved" or "within noise"); the `_none_`
cases — a trader's first keystroke, and the §7.1 gate's own shape —
improve the most (~30%) because they are the shape with the fewest
rows actually scanned and the most of their total cost is compile-time
catalog lookups rather than the scan itself. `_none_keys_textual_depth_2`
is unaffected as expected: with the key columns also textual, that
case is dominated by the row-scan `ILIKE` fallback the dictionary
rewrite cannot remove, not by catalog lookups. `1000000_rows_text_
none_depth_2` — the §7.1 gate — holds at 2.5109 ms, comfortably under
the 50 ms contract.

### Phase 4a: the two key columns stay in the text filter — what that costs, and the idea that did not pay

Decision (Matthew, 2026-09-07): `position_ref` and `instrument_ref` stay
textual in the demo schema. With the literal-list form merged
(`0314715`), the demo `tree` view at 1M rows, two generations, depth 2,
headless, medians of five, reads:

| needle | live | as-of | rows |
|---|---|---|---|
| `zzz` (matches nothing) | 53 ms | 105 ms | 1 |
| `spx` (an underlying) | 65 ms | 179 ms | 129 |
| `bk00` (ten books) | 45 ms | 105 ms | 441 |
| `0015` (a position-ref fragment) | 57 ms | 110 ms | 129 |
| `0` (matches nearly every key) | 67 ms | 158 ms | 705 |

Every live number is the two keys' per-row `ILIKE` across the three
measure grains, on top of a 12 ms no-text baseline; as-of adds its own
~45 ms baseline (archive ∪ live plus the generation predicate) and
scans the union.

**Tried and rejected: treating a key as its own dictionary.** The idea:
resolve a plain-string column's matches at compile time from the
coarsest grain carrying it (`select distinct position_ref … ilike ?
limit cap+1`), bind them as a list, drop the term when nothing matches,
fall back to the row scan past a cap. Probed on the same view: compile
went from 9 ms to 90–140 ms and every total got worse (live `zzz`
54 → 94 ms, as-of `spx` 179 → 205 ms). A key's cardinality is close to
the row count — there are as many position refs as positions — so
"resolving the dictionary" is the same scan the row test does, and
`compile_scope` runs once per grain plus the spine, so it ran four
times. An ENUM works because its vocabulary is hundreds; a key's is
hundreds of thousands. Not pursued.

**Levers that remain, in order of expected return:** (1) resolve
`existing_enum_types` and the dictionary matches once per
`compile_view` instead of once per `compile_scope` call (final-review
minor M3; ~4 resolves per requery today); (2) the as-of baseline —
attempted 2026-09-07 and rejected as racy (see "Phase 4a: the as-of
baseline — the generation predicate" below): reading only the tables
whose generations the as-of resolves to is exactly the static
side-drop that section's point 4 rules out, since compile and
execution run on separate connections and a publish landing between
them can move a generation to a side a side-dropping relation would
never look at again. What remains of this lever is the resolve's own
cost — see that section's open lever (a); (3) the schema, per desk.

### Phase 4a: the as-of baseline — the generation predicate

Found on a display: every requery under an as-of cost 50–70 ms before
any text filter was typed; live cost 12 ms. A headless probe (demo
schema, demo `tree` view, 1M rows, depth 2, an archive holding three
superseded generations per partition, medians of nine, ms end to end
through `DataService`, 2026-09-07) traced the cost to the
per-generation OR chain `generation_predicate` used to emit, and
established that a `gen_id` range plus a tuple semi-join, pushed into
both sides of the era relation, removes it without the race a static
side-drop would introduce:

| era | scope | today: `archive union all live` + OR chain | **this change** | static side-drop (racy, rejected) | live |
|---|---|---|---|---|---|
| pure archive | none | 68 | **27** | 26 | 12 |
| pure archive | text `zzz` | 121 | **68** | 68 | 38 |
| pure archive | text `spx` | 199 | **154** | 153 | 70 |
| mixed (one partition live) | none | 63 | **28** | 27 | |
| mixed | `zzz` | 119 | **68** | 65 | |
| mixed | `spx` | 199 | **153** | 155 | |
| pure live | none | 55 | **40** | 38 | |
| pure live | `zzz` | 100 | **79** | 77 | |
| pure live | `spx` | 178 | **166** | 163 | |

What the probe established, each point load-bearing for the design:

1. The OR chain is the cost: with no predicate at all the same union
   reads in 24 ms; the seventeen-disjunct OR evaluated per row over
   both tables costs ~30–40 ms. Reordering the disjuncts' terms
   (integer compares first) changes nothing.
2. A tuple semi-join alone is a hash lookup (27 ms no-text), but it
   runs *after* the text scan, so a text filter under as-of still
   scans every archived generation without a scan-level filter: `zzz`
   went 121 → 179 ms with the semi-join alone.
3. `gen_id between <lowest resolved> and <highest resolved>` is a
   plain scan filter DuckDB pushes to the table scan; zonemaps then
   skip whole row groups of other generations, and skip the whole
   *side* when it holds none of the resolved ids (a pure-archive
   era's live side, a pure-live era's archive side). With it, the
   race-free "read both sides" form matches a static side-drop within
   noise in every cell above. **(Superseded — see below: this range
   degenerates on a real archive and is replaced by an IN-list.)**
4. Dropping a side statically is **racy** and is rejected: `publish_file`
   is one transaction *per grain*, and compile (the resolve, on the
   service connection) and execution (a pool worker's connection) are
   separate statements. A publish landing between them moves a
   generation from live to archive at some grains and not others; a
   relation that read only the table the resolve saw would silently
   miss that partition for one frame. Reading both sides is what makes
   today's design correct under concurrent publish, and point 3 makes
   it free.
5. DuckDB's struct comparison treats NULL fields as equal (`select (1,
   NULL::varchar) in (select (1, NULL::varchar))` → true; `(1, 'x') in
   (select (1, NULL))` → false; verified on the pinned duckdb), so the
   plain tuple handles the NULL-book partition exactly with no
   `coalesce` sentinel — pinned by `as_of.rs`'s
   `the_predicate_selects_a_null_book_partition_from_either_side`.
6. A dynamic side-drop (an in-statement subquery deciding emptiness)
   is not skipped by DuckDB: 37 ms vs 24. Only the static range does it.

**Point 3's pruning is fixture-conditional, and the probe's own
fixture sits at the favourable end.** How much the range prunes
depends on how tight `[lo, hi]` is: the probe's archive holds three
superseded generations per partition, all on the same refresh
cadence, so `lo..hi` spans a handful of ids and zonemaps skip almost
everything else. A real desk has books refreshing on independent
schedules (§4.5) — one book last published Tuesday, another an hour
ago — which widens `[lo, hi]` toward the whole archive and the range's
pruning degenerates toward a no-op; the tuple semi-join alone then
carries the cost. The probe's own tuple-only reading (finding 2) is
that floor: `zzz` went 121 → 179 ms with no scan-level filter at all.
The "mixed" row above (28 ms) does not settle the wide-spread case —
it was measured with the same tight id spread as every other row in
the table.

The headless probe itself is not in this repo: a throwaway patch to
`benches/query.rs` (the demo schema and `tree` view pulled in via
`include_str!`, an archive built by four passes of `load_file` with
each pass's sentinel shifted 24 hours to force three superseded
generations per partition), run locally and discarded — the table
above is transcribed from its output, not reproducible from a
committed harness.

**This crate's own bench** (`crates/geode-data/benches/query.rs`,
`service_with_history`'s fixture: one archived generation per
partition, not three, ingested twice into a fresh 1M-row database;
`cargo bench -p geode-data --bench query --`, criterion, 20 samples,
before = base commit `bcc09af`, after = the fix above, same machine):

| Case | before | after |
|---|---|---|
| `1000000_rows_text_none_depth_2_asof` | 16.135 ms | **13.813 ms** (−14.9%) |
| `1000000_rows_scoped_depth_2_asof` | 36.538 ms | **26.544 ms** (−26.9%) |

Both hold the §7.1 <50 ms contract with room to spare; the smaller
absolute win than the headless probe's is the fixture, not the fix —
one superseded generation per partition here against three there, so
the OR chain this removes was shorter to begin with.

Two open levers, neither pursued here:

(a) **The resolve itself.** `resolve_generations` runs `select
distinct batch, book, gen_id, source_time` over every grain's archive
and live table, which the probe measured at 6 ms of compile on a
one-generation archive and 15 ms on a three-generation one — it scales
with the archive. Belongs in a small generations table maintained
inside the publish and sweep transactions, not scanned fresh per
compile.

(b) **The `spx` text case stays ~150 ms under as-of** because the
extra scan filter changes how DuckDB decorrelates the text filter's
membership probes (two 840k-row inner hash joins appear in the
profile) — a text-filter plan question, not a generation-predicate
one.

#### The range prefilter degenerates on a real archive — replaced by an IN-list

Found on a display (2026-09-07, `cargo run --release -p geode-app --
--demo 1000000`): a text-filter requery under as-of read 4823 ms. The
demo database (`$TMPDIR/geode-demo/1000000-42/geode.duckdb`) had
drifted, over an unrelated ingest defect (the ingest queue's own
handoff), to 20.8 GiB and 3051 file generations of 17 source files
that never changed — 27.7M archived position rows, 55M instrument,
116M underlying, 66M pair (~265M rows total). The live generations
resolved for an instant after every load were `[1, 2, 4, 7, 11, 17,
27, 43, 68, 108, 172, 295, 491, 767, 1210, 1934, 3051]` — 17 ids
spanning the whole 1..3051 id space.

The section above's point 3 flagged exactly this: "how much the range
prunes depends on how tight `[lo, hi]` is... a real desk has books
refreshing on independent schedules... which widens `[lo, hi]` toward
the whole archive." On this database that widening is total — `lo=1,
hi=3051` covers every generation ever written, so the range term
prunes nothing and the tuple semi-join alone carries the whole archive
scan. Measured directly (`select count(*) from
risk_snapshot_underlying_archive where …`, DuckDB's own `EXPLAIN`):

| predicate | rows | time | plan |
|---|---|---|---|
| `gen_id between 1 and 3051` | 116,039,965 | scans all | no scan filter |
| `gen_id in (1, 2, 4, …, 3051)` (17 ids) | 0 | 1.5 ms | `Filters: optional: gen_id IN (…)` |
| OR chain of `gen_id = g` | 0 | 1.4 ms | pushed as scan filter |

DuckDB pushes an IN-list of constants to the scan as an *optional*
filter and prunes by zonemap per id, regardless of how spread the ids
are — unlike the range, which only prunes when the ids happen to sit
close together. `generation_predicate` now emits `gen_id in
({ids})` (the distinct resolved ids, ascending) instead of the range,
keeping the tuple semi-join unchanged.

Headless, on the same 20.8 GiB/3051-generation database (demo schema,
demo `tree` view, depth 2, `DataService`, before this change):

| case | compile | exec | total |
|---|---|---|---|
| as-of, no text filter | 543 ms | 1075 ms | 1618 ms |
| as-of, `zzz` (matches nothing) | 522 ms | 5281 ms | 5803 ms |

(The display's 4823 ms end-to-end reading is the same order as the
headless `zzz` total above; the two were not measured in the same
process, so they are not expected to match exactly.) The exec half is
what the IN-list fixes — it is the archive scan the range predicate
had stopped pruning. The compile half is unaffected by this change and
is the open lever (a) above (`resolve_generations` scanning ~265M rows
for distinct generations) already flagged as remaining; it now has a
measured datapoint at real scale: **543 ms at ~3000 generations**,
against the 6 ms (one generation) and 15 ms (three generations)
figures the probe measured earlier — confirming it scales with the
size of the archive scanned, not just the row count of one query.

**This crate's own bench** (`crates/geode-data/benches/query.rs`,
`service_with_history`'s fixture — one archived generation per
partition, ingested twice into a fresh 1M-row database, so only two
generations are ever resolved; `cargo bench -p geode-data --bench
query -- "1000000_rows.*depth_2_asof"`, criterion, 20 samples, same
machine, before = the `gen_id between` form, after = this change):

| Case | before (range) | after (IN-list) |
|---|---|---|
| `1000000_rows_text_none_depth_2_asof` | 14.707 ms | 14.422 ms (criterion: −2.6%) |
| `1000000_rows_scoped_depth_2_asof` | 28.061 ms | 29.760 ms (criterion: +7.9%) |

As expected, this does not move on the bench's own fixture: with only
two resolved ids, a range and an IN-list prune identically (both cover
the same tight `[lo, hi]`), so the swings above are sampling noise on
a two-element predicate, not a real regression or win — the fixture
simply cannot reproduce the fixture-dependence the display exposed.
The IN-list's value is entirely in the case this bench does not model:
an archive whose resolved ids are spread across the id space, where
the range prunes nothing and the IN-list still does.

**Verified on the display, 2026-09-07 evening:** with both fixes merged
(`f6ac302`) and the demo database deleted and rebuilt, the same
as-of text-filter requery that read 4823 ms reads **47 ms + 4 ms**
(requery + paint, the perf overlay's figures) — a hundredfold, and
back inside the §7.1 budget with the paint included. That is a
one-generation database; the headless figures above (68 ms `zzz`,
133 ms `spx`) were taken through the bench harness on a different
process and needle, so the display reading is the one of record.

### The generations summary table

`resolve_generations` used to answer "which generation did each
partition hold at T" by running `select distinct batch, book, gen_id,
source_time` over **every** grain's archive and live table and
windowing the union — a full scan of the whole database per as-of
requery. Measured (§ above, "Phase 4a: the as-of baseline"): 6 ms of
compile on a one-generation demo database, 15 ms at three generations,
**543 ms at ~3000 generations** (265M archived rows). It scaled with
the archive, and a real desk's archive grows all day.

The set of generations is tiny — partitions × retained generations,
hundreds to low thousands of rows — and every event that changes it is
already a transaction this crate owns: `publish_file` adds a
generation, `retention::sweep` removes them. So the generation set is
now maintained as a table, `generations` (dataset, batch, book, gen_id,
source_time), inside those same transactions, and `resolve_generations`
reads it directly by dataset rather than scanning tables at all. The
data tables stay the truth for *rows*; the summary is the truth for
*which generations exist*, and it can never disagree with the tables
because it changes in the same transaction they do.
`store::ddl::rebuild_generations` rebuilds it from the data tables —
the migration path for a database written before this table existed,
and the tests' oracle.

**`crates/geode-data/benches/query.rs`, new `resolve` group**
(`resolve_generations_50_generations`: a fresh 20,000-row database
ingested 50 times with the sentinel shifted one hour per pass, timed at
an instant after every load; criterion, 20 samples, before = base
commit `1046b0f` — where `resolve_generations` took the table list from
`history_of` and scanned it — after = this change, same machine):

| Case | before | after |
|---|---|---|
| `resolve_generations_50_generations` | 7.545 ms | **709.2 µs** (−90.6%) |

Resolution no longer scales with the archive: at 50 generations the
table-scan form already cost more than a millisecond of the §7.1
budget on a 20,000-row database with no archived history to speak of
beyond the generations themselves; the summary read is a lookup
against a few-hundred-row table regardless of how large the archive
behind it grows.

**The two 1M-row as-of cases, re-run before/after** (`service_with_history`'s
fixture — one archived generation per partition, ingested twice into a
fresh 1M-row database, so only two generations are ever resolved; same
before/after commits, same machine):

| Case | before | after |
|---|---|---|
| `1000000_rows_text_none_depth_2_asof` | 12.70 ms | **4.82 ms** (−62.0%) |
| `1000000_rows_scoped_depth_2_asof` | 27.13 ms | **19.57 ms** (−28.0%) |

Both hold the §7.1 <50 ms contract with more room than before. This
fixture only ever resolves two generations, so the win here is the
compile share the old baseline section flagged as remaining ("The
resolve itself... belongs in a small generations table maintained
inside the publish and sweep transactions") — not the archive-scan
cost the earlier IN-list fix targeted, which this fixture never paid
either. The larger win from a real, high-generation-count archive (the
543 ms case above) is exactly what the summary table exists for, and
is no longer reachable through this crate's own benches: the compile
that used to scale with 3000 generations is now a fixed-cost lookup.

## Phase 4b: diagnostics

Phase 4b (`docs/superpowers/specs/2026-09-06-geode-phase-4-frame-features-design.md`
§4) adds an always-on log ring and a `geode-diagnostics` tile over it,
both explicitly subject to the charter's per-frame-churn rule. This
section records the allocation and rebuild contracts that keep them off
the render thread's budget, and the one display reading that would
prove it — not yet taken; template rows below. (A source's *reported*
health is a correctness question, not a performance one, so it is
covered in the spec's §4.4 as-built note, not here — final review round
3, NEW-4: it is the worse of two independently-tracked lanes, discovery
and load, precisely so a routine, content-blind clean poll can never
silently clear a real, unfixed problem a publish set.)

**The ring reader's allocation contract.** `geode_core::log::Ring::
drain_since` is the one thing a following diagnostics tile calls every
time it might have new log lines, so its cost model is pinned by tests,
not just documented:

- `a_hit_allocates_nothing_in_the_reader` (`crates/geode-core/src/log/mod.rs`)
  drains once, records the returned `Vec`'s capacity, pushes more
  records, drains again for the same count, and asserts the capacity
  did not grow — the reader owns its buffer and reuses it, so a tile
  following the tail allocates only for records genuinely newer than
  its `since`, never for the act of reading.
- `drain_since` itself walks backward from the newest slot and stops
  the instant it reaches a record at-or-before `since` (or an unwritten
  pre-wrap slot), rather than scanning all 4,096 slots on every call —
  a `#[cfg(test)]` `Ring::drain_visits` counter proves the bound
  directly rather than inferring it from timing.
- The one real allocation on a hit is each matching record's `message:
  String` clone, made while the ring's mutex is held — the mutex is
  never held for *formatting* (that happens on the emitting thread,
  before `push`), only for the copy.

**The no-rebuild-on-bare-notify discipline.** Every diagnostics section
rebuilds only when an observed version actually changed, not on every
`cx.notify()` a nearby entity happens to fire — a ring push carries no
notify of its own, so a real caller relies on some other `Diagnostics`/
`Frame` version bump (the reload-poll tick, a health note, a config
reload) landing nearby to pick up new log lines, and that same version
comparison must not rebuild on a notify that changed nothing.
`an_unchanged_entity_does_not_rebuild_rows` (`crates/geode-diagnostics/
src/tile.rs`) pins this the hard way: it calls `cx.notify()` on both
`diagnostics` and `frame` with no real mutation behind either call and
asserts `rebuild_count` does not move. The frame-driven half of that
comparison narrows to exactly the two `FrameVersions` fields any
section reads (`as_of`, `config`) — a scope-only or grouping-only frame
change is not a reason for this tile to rebuild.

**Per-population versions (final-review fix, MAJ-4).** The whole-branch
review found that this discipline held for the *entity's own* version
but not fully: `Diagnostics` had one combined `version()` counter, so
ANY mutation — including the perf-only reload-tick copy of the frame
histogram, which fires roughly every 500ms while any diagnostics tile
is visible — bumped it, and the tile's one `cx.observe` compared that
single counter regardless of which of the five sections was showing.
For four sections that comparison was cheap to redo; for `config` it
was not — `sections::config_rows` walks every loaded doc's leaves
(several allocations per leaf: the walked path, the value's `to_
string`, `explain`'s own probe, the row's `format!`), and was being
redone twice a second whenever a `config` tile sat open beside anything
painting frames, whether or not the config actually changed.

The fix: `Diagnostics` gained `versions: DiagVersions { sources, data,
config, log_levels, perf }` (`crates/geode-shell/src/diagnostics.rs`),
one counter per section, each bumped only by the mutators that
section's row builder reads — `note_config`/a changed `note_data_
diagnostics` bump `config` (despite `data_diagnostics`' name, it is
`config_rows` that renders it); `describe_source`/`note_health`/
`note_polled` bump `sources`; `note_published`/`set_catalog` bump
`data`; `request_level`/`set_levels` bump `log_levels`; `refresh_frame_
hist`/`note_dropped` bump `perf`. `Diagnostics::version()` is untouched
and keeps bumping on every mutation, unrelated to this — it still backs
`summary()`'s cache. `DiagnosticsTile`'s two `cx.observe` closures
(`crates/geode-diagnostics/src/tile.rs`) now compare only the
version(s) the *current* section reads — `diag_version_for_section`
for the diagnostics-entity half, plus the ring's own `latest_seq` for
the log section specifically (not a `Diagnostics` version at all); the
frame-driven half compares `as_of` only while showing `Data` and
`config` only while showing `Config`. A perf-only tick while showing
`config` now touches neither counter the config comparison reads, so
`rebuild()` — and `config_rows`'s walk — does not run.

Pinned by `refresh_frame_hist_does_not_rebuild_the_config_section`
(a perf-only bump leaves the config section's `rebuild_count`
untouched) and `note_config_rebuilds_the_config_section` (a real config
change still does) in `crates/geode-diagnostics/src/tile.rs`.

**The config section's own cost, measured (display-free proxy).** No
display was available to take the reading the "Display recipe" section
below describes, so — as MAJ-4's ruling asks — `config_rows` was timed
directly against the largest config this repo ships: the `--demo`
layer's five docs (`app`, `datasets`, `dimensions`, `groupings`,
`views`) plus the compiled-in builtin keymap (the single biggest doc in
a real session — one leaf per binding key, after the earlier MAJ-8 fix
recurses into `[[bindings]]`'s array). Recipe:

```sh
cargo test -p geode-diagnostics --lib \
  sections::tests::config_rows_on_the_demo_config_stays_under_budget \
  -- --nocapture
```

| build | rows produced | `config_rows` elapsed |
| --- | --- | --- |
| debug (`cargo test`, unoptimized) | 591 | ~0.7-1.3ms (five runs, warm cache) |

Well inside the render thread's 8ms pure-UI budget even unoptimized and
even paid on every rebuild — the periodic-tick waste MAJ-4 fixed was
real (twice a second regardless of relevance), but the walk itself was
never close to the budget on a config this size. The test asserts a
generous 10ms sanity bound, not this measured number, so it does not
flake on a slower CI runner; a maintainer whose desk config grows much
larger than the demo config's five docs plus the builtin keymap should
re-run the recipe above rather than trust this row indefinitely.

**The histogram copy cadence.** `ShellView::perf: FrameHistogram` stays
the value the overlay and the render path read every frame;
`Diagnostics.frame_hist` is a *copy*, refreshed on the shell's existing
~500 ms reload-poll tick and only while `Diagnostics.watchers() > 0` —
an open-but-invisible or closed diagnostics tile costs nothing here.
`refresh_frame_hist` also compares before copying, so even a watched
tile's copy is a no-op once the histogram stops changing between ticks.
Pinned by `the_frame_histogram_is_copied_only_while_watched` and
`refresh_frame_hist_is_a_no_op_when_the_histogram_is_unchanged`
(`crates/geode-shell/src/diagnostics.rs`).

**Display recipe — superseded.** The tile this recipe opens no longer
exists (the diagnostics page replaced it on 2026-09-27); the equivalent
reading for the page is the painted-frame item listed under "Diagnostics
page" at the end of this log. The template is kept for the method.
The claim to
verify: opening a diagnostics tile with the log section following (so
it is draining the ring on every relevant tick) must not move the
frame-time histogram's p95 against a baseline with no diagnostics tile
open. Recipe, matching the Phase 3/4a sections' convention above:

```sh
cargo run --release -p geode-app -- --demo 1000000
```

1. With no diagnostics tile open, reset the overlay's counters
   (`perf::reset`, palette-only) and hold `j` in a blotter tile for a
   few seconds the way the Phase 3 wide-view reading above did; read
   **p50**/**p95**/**max** before releasing.
2. Open a diagnostics tile (`ctrl+k` → "Diagnostics: Split", or the
   status bar's diagnostics-summary click), switch to the `log`
   section (`]`/`[` or `:section log`) so it is following the tail,
   trigger some log activity (an ingest tick, a `:level` change),
   `perf::reset` again, and repeat the same `j` hold in the blotter
   tile.
3. Compare the two p95 readings — the claim holds if they agree within
   noise (same order of magnitude as the Phase 3 wide-view p95 above,
   not a new tail introduced by the diagnostics tile's background
   copying).

| reading | where read | value |
| --- | --- | --- |
| baseline `j`-hold, no diagnostics tile, p50/p95/max | overlay, counters reset before the hold | *(template — not yet measured)* |
| same hold, diagnostics tile open + log section following, p50/p95/max | overlay, counters reset before the hold | *(template — not yet measured)* |
| verdict: does the diagnostics tile move p95? | comparison of the two rows above | *(template — not yet measured)* |

## Phase 4c: what a config-dialog keystroke costs, and what its flush costs

Config dialogs have no save key (spec §3.2/§7.1). A keystroke changes the
draft — which is what the edit stage paints, so the dialog responds with
nothing in between — and everything else rides one 250 ms debounce: the
merge, `hot_reload::apply_reload`, and the file write.

**The split is the point, and it was measured before it was chosen.** An
earlier build applied the merged config on every keystroke. That is
affordable as arithmetic and not as behaviour: `apply_reload` emits
`ShellEvent::ConfigReloaded`, the app bridge turns it into fresh
`ViewSpec`s, and **every blotter tile requeries** — a §7.1 <50 ms
operation at 1M rows, fired at the OS key-repeat rate under a held key.
The debounce now carries the fan-out and the write together, because both
are "the rest of the world catches up".

### Per keystroke (`cargo bench -p geode-shell -- config_edit`)

| Bench | time |
|---|---|
| `config_edit/keystroke_toggle_validate_render` | **37.1 µs** |

That is the keystroke's whole pure core: toggle the row, re-run
`Domain::validate`, render the object for both destinations, and turn
each into the value memory and the file both take. **0.46% of the 8 ms
pure-UI budget.** It is an over-estimate on purpose — the bench rebuilds
the `Draft` from the config each iteration, which a real keystroke does
not do (it mutates the draft it already has) — so the real figure is
lower than the one recorded here.

No merge and no `apply_reload` appear in that number, because after this
design neither runs on a keystroke.

### Per flush, once per 250 ms

| Bench | time |
|---|---|
| `geode-shell` `config_edit/flush_merge_full_layer` | **70.0 µs** |
| `geode-shell` `config_edit/flush_build_keymap` | **20.3 µs** |
| `geode-core` `config_merge/config_from_docs_demo_desk` | 63.7 µs |
| `geode-core` `config_merge/config_all_docs_then_from_docs` | 64.0 µs |

`flush_merge_full_layer` is the honest merge number: a builtin layer
carrying the **real** `BUILTIN_KEYMAP` — by far the largest document in
the config model — plus the demo desk's 7.8 KB `datasets.toml` and 6.8 KB
`views.toml`. The two `geode-core` numbers measure the demo desk without
the keymap and are therefore a mild underestimate; that crate cannot
reach `BUILTIN_KEYMAP` (it must not depend on `geode-shell`), which is
why the keymap-inclusive figure lives in the shell's own bench. The
`all_docs` + `from_docs` pair being within noise of `from_docs` alone
says cloning the documents out of the live `Config` is free at this
scale.

`flush_build_keymap` is measured because `apply_reload` rebuilds the
keymap **unconditionally**, and this design moved that code from a 500 ms
poll into an interactive path. "It was already in the reload path" stops
being an excuse the moment the caller changes; at 20 µs once per 250 ms
it is not a concern, but that is now a number rather than an assumption.

A whole flush is therefore on the order of 100 µs of pure work plus the
file write, once per debounce window — nowhere near a frame, and off the
keystroke entirely.

### Not measured

`apply_reload`'s remaining work (`docs_equal` over each doc, the theme
and pickable re-derives, the frame updates) and the fan-out it triggers.
The fan-out's cost **is** the blotter requery already budgeted by §7.1;
what this design changed is how often it can fire, which is now bounded
at one per 250 ms rather than one per key repeat.

## Market-data documents (spec §5.4/§6, Part 2, Task 11)

Two costs on the document path, both `--release`, DuckDB 1.10505
bundled, criterion medians, an M-series Mac. Read them side by side:
the first is the receiver thread's cost, in memory, before a document
ever reaches the runner; the second is the runner's cost after.

**Parse and write** (`cargo bench -p geode-documents`, `benches/cvi.rs`)
— `CviKind::parse`/`write` at two grid shapes: 20 terms × 30 nodes (600
rows, a single underlying's surface, the shape a panel shows) and 200 ×
300 (60,000 rows, well past anything the desk sends):

| Benchmark | Result |
|---|---|
| `cvi/write/20x30` | 48.4 µs |
| `cvi/parse/20x30` | 67.8 µs |
| `cvi/write/200x300` | 4.65 ms |
| `cvi/parse/200x300` | 5.85 ms |

(Task 4's own fix-wave numbers — write 47.3 µs/4.60 ms, parse 68.6
µs/5.73 ms — read within normal run-to-run noise of these.)

**Publish** (`cargo bench -p geode-data --bench publish_document`, new)
— `publish_document` at the same two shapes, staging and publishing
through a real temporary `Store` (schema applied, catalog tables
ensured). Each iteration gets its own store, the same per-iteration
pattern `benches/ingest.rs` uses, so a hundred-odd publishes never share
one growing archive that would skew later samples; the *timed* half of
each iteration is a second publish of the same key over an untimed
first one — a live document overwriting a live document, which is the
demo bus's steady state, not the one-off, less interesting cost of the
first insert into an empty table:

| Benchmark | Result |
|---|---|
| `publish_document/20x30` | 5.32 ms |
| `publish_document/200x300` | 146.8 ms |

Publish costs roughly two orders of magnitude more than parse+write at
600 rows (5.32 ms against 116 µs) narrowing to about one order of
magnitude at 60,000 rows (146.8 ms against 10.5 ms) — expected, since
`publish_document` does real synchronous disk I/O per row (a `create or
replace` staging table, one `Appender::append_row` call per row, then
`publish_file`'s own transaction moving staging into the live/archive
tables and refreshing the key dimension's ENUM dictionary) where
`CviKind::parse`/`write` never touch a disk. The two benchmarks are not
directly additive into an end-to-end document latency — `parse`+`write`
measures the wire format alone and `publish_document` starts from
in-memory `DocumentRows`, the same handoff shape the receiver thread
hands the runner — but together they show where the document path's
cost actually sits: overwhelmingly in the store, not the parser.

### Archive growth at the demo cadence

The demo bus (`geode-app::demo_bus`) publishes one document roughly
every `cadence` (5 s, the shipped constant), independent of key count —
it advances one key per wait rather than waiting once per key, so any
one key's own republish period is about `cadence × keys` (ten
underlyings ⇒ ~50 s per key at the shipped cadence), while the
*overall* rate of new generations is one per ~5 s throughout.

A one-off measurement, same method as `publish_document` above but at
the demo's own real document shape — `CviGenerator`'s 8 listed expiries
× 12 fixed nodes = 96 rows per document, smaller than either bench's
synthetic grid — published one key 50 times running into a fresh store
and read the `.duckdb` file's own size (after `checkpoint`) before and
after: **2,097,152 bytes of growth over 50 publishes, ~41.9 KB per
generation.** (That figure is dominated by DuckDB's own block/segment
allocation granularity rather than the ~1.5 KB of raw column payload a
96-row document actually carries — the round number, exactly 2 MiB
over 50 publishes, is itself the tell.) At one new generation roughly
every 5 s that is about **8.2 KB/s ⇒ ~29.5 MB/hour ⇒ ~708 MB/day** of
continuous `--demo` running.

**Known gap, recorded rather than fixed here:** nothing in the document
path currently sweeps or retires old generations — Phase 2a's
retention/sweep machinery (`store::retention`) runs over directory
sources' `file_generations`, and a subscribed source's document
generations are not yet wired into it — so a `--demo` session (or a
real desk's subscribed source) left running indefinitely would grow its
archive unbounded. Out of this task's scope; a candidate for whichever
of Parts 3–4 next touches retention.

## Market-data panel (spec §8, Part 3)

The panel's pure core (`geode_marketdata::core`) is what the tile's
render path leans on to stay under the §7.1 8 ms pure-UI budget:
`MatrixModel::build` runs once per delivery or per draft change, never
in `render`, and a frame only ever clones the prepared `SharedString`s
it already produced.

**Bench** (`cargo bench -p geode-marketdata`, criterion medians,
`--release`, an M-series Mac):

| Benchmark | Result |
|---|---|
| `marketdata_core/model_build_pivot_20x30` (CVI's own shape, `Columns::Axis`) | 285 µs |
| `marketdata_core/model_build_values_10000x5` (a broad-index dividend schedule, `Columns::Values`) | 8.18 ms |
| `marketdata_core/draft_rebase_1000_edits` (onto the 10,000×5 model) | 297 µs |

CVI, the only panel that exists at Part 3, costs 285 µs per build and
is nowhere near the budget. The 10,000×5 flat build at 8.18 ms is the
number that mattered: it is the whole of the §7.1 8 ms pure-UI budget
on its own, and at Part 3 it was not only a delivery-path cost —
`commit_edit` called `rebuild_model` on the committing keystroke, so a
schedule-shaped panel at that size would have blown the keystroke
budget on every cell edit.

**The dividend-schedule plan (2026-09-19, spec §4.5/§5.2/§8) built the
rule above for a CELL commit only: `MarketDataTile::commit_cell_value`
now calls `MatrixModel::patch_cell` — re-preparing that one cell —
instead of `rebuild_model`.** `DIVIDEND` is the schedule-shaped panel
this closes the gap for. `patch_cell` has exactly one production call
site (`commit_cell_value`, which `step_choice`'s `space`/`shift+space`
also commits through); every other path still calls `build`/
`rebuild_model` wholesale: a delivery, a draft restore, row
insert/delete (`o`/`shift+o`/`d d`, since splicing a row shifts every
row below it and `patch_cell` only re-prepares one already-positioned
cell) — **and `:bump`, on purpose (controller ruling): a row or column
bump touches many cells at once, `write_steps` and the model rebuild
together in one call, and patching each touched cell individually is a
perf follow-up, not built here.** A column `:bump` on a 10,000-row
schedule therefore still pays the full flat-build cost below, exactly
as a delivery does — only a single cell's own commit is cheap now.

| Benchmark | Result |
|---|---|
| `marketdata_core/patch_cell_pivot_20x30` (a committed cell, CVI's shape) | 92 ns |
| `marketdata_core/patch_cell_values_10000x5` (a committed cell, the flat shape) | 116 ns |
| `marketdata_core/model_build_values_10000x5_100_rows_spliced` (`build`, 100 rows inserted — the cost `o`/`shift+o`/`d d`, a delivery, or a column `:bump` all still pay at this size) | 6.09 ms |
| `marketdata_core/draft_rebase_1000_edits_100_rows` (`Draft::rebase`, 1,000 cell edits + 100 row inserts) | 233 µs |

`patch_cell` costs low hundreds of nanoseconds at both shapes — four to
five orders of magnitude under the 8 ms budget — because it re-prepares
exactly one cell's text, value and state rather than the whole grid; a
trader typing into a 10,000-row schedule now pays that near-zero cost
per keystroke on the cell they are actually editing, the same as CVI's
own 20×30 grid always did. `:bump`, row insert/delete and a delivery
all still call `build`/`rebuild_model`, so
`model_build_values_10000x5_100_rows_spliced` is the number that bounds
all three at 10,000 rows — inside the §7.1 8 ms budget but not by a
wide margin, as the un-spliced flat build already was not (the same
pass's own `model_build_values_10000x5` reading, for a direct same-run
comparison, was 5.94 ms — 100 spliced rows costing about 2.5% more over
10,000 document rows, the expected direction). Likewise
`draft_rebase_1000_edits_100_rows` (233 µs) cost about 8% more than the
same pass's own plain-cell `draft_rebase_1000_edits` reading (216 µs) —
rows resolve by label through the same kind of map lookup cells do, so
100 more rows add a little, not a lot.

**A caveat this session's own numbers earn, named rather than hidden:**
an earlier pass through these same four benchmarks read the two
"more work" variants FASTER than their own same-session baselines —
`model_build_values_10000x5` at 9.83 ms against
`…_100_rows_spliced` at 6.66 ms, and `draft_rebase_1000_edits` at
400 µs against `…_100_rows` at 216 µs — which cannot be a real
speed-up from doing more work and is host contention, not signal (this
session's host ran a shared load average that swung from ~20 to ~85
across reruns of these same, otherwise-unchanged benchmarks). The
numbers above are a later re-run where the baseline and its "more
work" variant finally read in the expected direction, but they are
**still not a trustworthy point estimate on their own** — read every
figure in this section as an order-of-magnitude reading taken on a
busy shared machine, and re-measure on a quiet host before relying on
any of it for a real regression check.

**Paint at 10,000 rows — not yet measured on a display.** The
implementation sandbox has no window; the display recipe below is the
template, in the shape Phase 3's "the painted frame" section used for
the blotter.

The body is gpui-component's table since the user's ruling of
2026-09-14 ("visual unity" with the blotter — spec §8.8), so the paint
being measured is the same component the blotter's own painted-frame
numbers were taken through: one `render_td` per visible cell over the
prepared `MatrixModel`, only the visible rows laid out. Nothing above
changes — `MatrixModel::build` is untouched, and so is every site that
calls it — and nothing below has been measured yet.

| reading | where read | value |
| --- | --- | --- |
| frame time, `DataTable` paint over a 10,000-row `Columns::Values` panel, p50/p95/max | perf overlay, counters reset before scrolling | *(template — not yet measured)* |

## Timeseries (spec §4.4, Part 1)

`append_series`'s store-side cost, `--release`, DuckDB 1.10505 bundled,
criterion medians, an M-series Mac (`cargo bench -p geode-data --bench
append_series`, new). Each iteration gets its own temporary `Store` —
schema applied from the real `examples/demo-config/datasets.toml`,
catalog tables ensured — the per-iteration pattern `benches/ingest.rs`
and `benches/publish_document.rs` both use. The untimed setup half
appends the span once already; the **timed** half appends a second set
of values over the same timestamps under a later `received_at`, so what
is measured is the steady state a fetch source produces all day: stage,
dedupe against the live row per `(source, series_id, ts)`, insert what
remains, write the coverage row, and run the pair's retention sweep
inside the same transaction (the demo dataset declares `retention =
"7d"`, and §4.7 as built puts the sweep in `append_series` itself).

| Benchmark | Result |
|---|---|
| `append_series/390` (one session of minute bars, the widening a chart asks for) | 2.52 ms |
| `append_series/196560` (two years of them, a first load) | 249 ms |

The 390-row figure is the one the interactive path pays: it is off the
UI thread entirely (the ingest runner's series lane), and the tile hears
back through `DataEvent::SeriesFetched`, so it is nowhere near the §7.1
8 ms pure-UI budget even if it were on it. The 196,560-row figure is a
**first-load bound, not a steady-state one** — a pair is loaded whole
once and afterwards only its gaps are asked for, because
`DataService::fetch` subtracts the coverage table before queueing any
work.

**Known gap, recorded rather than fixed here:** the dedupe's
`arg_max(value, received_at) … group by ts` aggregates every stored
version of the pair on every append, so the timed cost above grows with
what is already stored, not only with what is being appended. It does
not show at
these sizes against the insert's own cost, and the append is off the UI
thread, so nothing is done about it now — but an index on `(source,
series_id, ts)` or partitioning the series table is what would be
reached for if a desk ever reports an append that drags, and this is the
line it would be measured against.

## Timeseries series query (spec §6, Part 2)

What a chart tile's round trip costs at a million rows, `--release`,
DuckDB 1.10505 bundled, criterion medians, an M-series Mac (`cargo bench
-p geode-data --bench series_query`, new). One temporary store per bench
process holds four identities of one-minute bars over a year — 250
sessions × 1,000 bars each, 1,000,000 rows — appended through
`append_series` in day-sized chunks in the untimed setup, the shape a
fetch source produces. The schema is the minimal `[series] family =
"series"` rather than the demo config's, because that one declares
`retention`/`history` and the history sweep would delete a year of 2025
bars out from under the bench.

The **timed** half is `DataService::series` plus the wait for its
`DataEvent::Series`: compilation, the cap check, the pool hop, DuckDB's
work, the struct-of-arrays read and the sink's per-slot health lookup —
the whole trip a tile pays, which is what the §7.1 50 ms requery budget
is written about. Every request is `AsOf::Live`; `window` is the range,
so the stats case computes over every bucket it paints.

| Benchmark | Result |
|---|---|
| `series_query/1_slot_1d_1y` (one slot, `1d` over the year: that slot's 250,000 rows folded into the ~300 daily buckets its sessions touch — each 1,000-minute session starts at 14:30 UTC and spills into the next UTC day) | 3.64 ms |
| `series_query/4_slots_plus_ratio_1d_1y` (four slots plus an `s1 / s2` expression, `1d` over the year: the whole million rows) | 9.56 ms |
| `series_query/2_slots_1m_1mo_with_stats` (two slots, `1m` over a month — a 44,640-bucket span, ~23,000 of them carrying a bar — with three percentiles and 40 bins) | 14.5 ms |

All three are inside §7.1's 50 ms, the widest by a factor of three. The
`1d` pair is the shape a trader holds open all day; the `1m` one is the
zoomed case, and it is the only one that pays for stats.

**Known gap, recorded rather than fixed here:** every stats statement
carries the same CTE prefix as the points statement and so **re-runs the
bucketing**. A request with percentiles and density on runs `1 + 2k`
bucketing passes for `k` slots — the third row above is five passes over
its month, not one, and is still 14.5 ms because the month is small
relative to the table. The fix, if a desk ever asks for stats over a
range where this bites, is a single statement with `grouping sets` (or
one CTE materialised and read three times) so the bucketing happens
once; it is not done now because the measured cost is a third of the
budget and the per-statement shape is what makes each slot's stats
independently testable.

## Line pricer core (spec §12, Part 2)

`cargo bench -p geode-pricer`, criterion medians, `--release`, an M-series
Mac. The sheet is 1,000 rows (every tenth a two-leg callspread), the
shape spec §8.2 sizes the grid model for. Nothing here paints; the grid
model bench is Part 3's.

| Benchmark | What it is | Result |
|---|---|---|
| `parse_1000_lines` | the shorthand parser over 1,000 typed lines | 270 µs |
| `apply_undo_sheet_shift_1000` | one sheet-wide shift and its undo: every line's request compared twice, ~1,000 lines staled each way | 1.52 ms |
| `apply_undo_set_instrument_1000` | one cell edit and its undo at 1,000 lines: the per-keystroke cost | 6.66 µs |
| `deliver_all_1000` | one full reprice landing as a batch: ~900 results installed and ONE `fold_packages` | 175 µs |
| `to_rows_from_rows_1000` | the autosave's document build plus a restore's rebuild through `Edit::Restore` | 1.14 ms |

Budget: the per-keystroke figure is what §7's 8 ms pure-UI budget
constrains (an edit happens on the UI thread before the frame that shows
it); the sheet-wide edit is the worst single keystroke (`:shift spot 2`).
`parse` runs once per `enter` in entry mode. The round trip runs once per
autosave (`to_rows`, Part 4's write-behind) and once per restore. All
five medians are well inside the 8 ms budget, the sheet-wide shift (the
worst of the five) leaving over 6 ms of headroom before the grid model's
own paint cost is even added in.

`deliver_all` is the door a whole batch of results lands through, and
the figure above is why it exists: the per-line `deliver` folds every
package on the sheet per landing, which is right for a single result
and quadratic for a batch (one full reprice of a 1,200-row sheet
measured 3.37 ms that way against 175 µs here). Part 3's
`Delivery::Price` arm calls `deliver_all`; `deliver` stays the
single-result form.

## Line pricer tile (spec §12, Part 3)

`cargo bench -p geode-pricer -- grid_build_1000`, criterion median,
benchmark profile, an M-series Mac with other cargo builds having run
earlier in the session (23 of 100 samples were outliers, all high). The
fixture is the Part 2 sheet: 1,000 rows, every tenth a two-leg callspread,
every line answered through one `deliver_all`, every package open, the
bundled `vanilla` view, UTC clock, no entry placeholder.

| Benchmark | What it is | Result |
|---|---|---|
| `grid_build_1000` | one whole `GridModel::build`: every visible row's shorthand label and every cell's text and state | 1.85 ms (1.8517 ms; interval 1.8355–1.8718 ms) |

Budget: the 8 ms pure-UI budget. The tile rebuilds the model on every
edit, delivery, expansion, view, clock and entry change (never in
render), so this is added to the keystroke's own edit cost — the worst
case, a sheet-wide `:shift` (1.52 ms above) plus the rebuild, stays
under 3.5 ms. Paints are a per-theme memo and are not in this figure; a
theme switch re-derives the memo and rebuilds nothing.

### Package row cells aggregated (2026-09-27)

`cargo bench -p geode-pricer --bench core -- grid_build_1000`, the same
fixture as above, after package rows began painting their legs' distinct
values joined with `/` (every text column, the package quantity or leg
list, and shift groups by spelled text) instead of blank text cells. Apple
M5 Pro, rustc 1.96.0, bench profile, 100 samples. **The machine was loaded**
(load average 20–25 from concurrent builds in other checkouts). The first
run, taken while this checkout's own bench build was still finishing, read
1.78 ms (interval 1.65–1.94 ms, 13 outliers); the rerun below is the value
of record.

| Benchmark | What it is | Result |
|---|---|---|
| `grid_build_1000` | one whole `GridModel::build`, 100 two-leg package rows aggregated | 1.42 ms (1.4213 ms; interval 1.4047–1.4404 ms, 6 outliers) |

Aggregation does not show above the earlier 1.85 ms reading; the two runs
differ by machine load more than by the change. Well inside the 8 ms
budget.

### Pricer vocabulary: 28 measures (2026-09-28)

`cargo bench -p geode-pricer -- 'grid_build_1000|deliver_all_1000'` (Criterion
takes one filter, so both ran under one regex), the same fixture as above,
after `PriceResult` widened from six measures to the fourteen bumped
measures with a USD twin each: a `Currency` and two `[f64; 14]` arrays, 232
bytes per result, 28 result columns in the paint vocabulary. Apple M5 Pro,
rustc 1.96.0, bench profile, 100 samples. **The machine was loaded** (load
average 9–15, this checkout's own bench build having finished moments
before).

| Benchmark | What it is | Result |
|---|---|---|
| `deliver_all_1000` | one full reprice landing as a batch: ~900 232-byte results installed and ONE `fold_packages` over 28 arrays | 179 µs (179.23 µs; interval 178.92–179.57 µs, 2 outliers) |
| `grid_build_1000` | one whole `GridModel::build`, 100 two-leg package rows aggregated, the bundled `vanilla` view | 1.09 ms (1.0947 ms; interval 1.0831–1.1055 ms, 17 outliers, 16 high severe) |

`deliver_all_1000` moved from 175 µs to 179 µs: the wider copy and the
28-array fold cost about 4 µs across 900 results, inside run-to-run noise.
`grid_build_1000` reads 1.09 ms against the previous 1.42 ms; the earlier
run was taken under a heavier load (20–25), so the difference is not
attributed to the change. Both stay well inside the 8 ms budget; the
worst keystroke (a sheet-wide `:shift` plus the rebuild) reads under 3 ms
from the earlier six-measure `apply_undo_sheet_shift_1000` figure, which
was not re-run here.

### Pricer tree column (2026-09-28)

`cargo bench -p geode-pricer --bench core -- grid_build_1000`, the same
fixture, after column 0 became a connector tree: the build now formats a
summary (distinct expiries, then distinct strikes) and a leg-count note per
package, and a line or leg shares one shorthand string between its painted
`text` and its `search` key. Apple M5 Pro, rustc 1.96.0, bench profile, 100
samples, run twice back to back. **The machine was heavily loaded** (load
average 45 then 27 over one minute, 64–81 over five and fifteen; other
sessions' builds).

| Run | Load (1 min) | Result |
|---|---|---|
| first | 45 → 36 | 2.15 ms (2.1511 ms; interval 2.0513–2.2641 ms, 2 outliers) |
| second | 34 → 27 | 1.66 ms (1.6560 ms; interval 1.6247–1.6956 ms, 8 high severe) |

Against slice 1's 1.09 ms (taken at load 9–15) the reading is higher, but
the two runs here differ by 23% with only the load changing, so the
difference is not attributed to the change; a quiet-machine re-run is
owed. Either reading is well inside the 8 ms budget.

### Pricer scope (2026-09-28)

What a frame-scope change or any rebuild under a scope costs the pricer
tile: `apply_scope` evaluates the scope over every sheet line, then
`GridModel::build` prepares the rows it shows. Fixture: the `grid_build_1000`
sheet (1,000 entries, 1,200 rows, every package open, every line priced), the
scope `underlying_ref = 'SPX' and strike >= 5000 and npv != 0` plus the text
filter `spx`, which hides about half the lines (`strike >= 5000`; the bench
asserts between 400 and 700 hidden). Every line matches the text filter, so
all nine textual columns are read on every line. `expr_evaluate_row` is the
core evaluator alone: a three-term expression and a text filter over one
kept row of a four-column dataset.

`cargo bench -p geode-pricer --bench core -- 'apply_scope_1000|grid_build_1000'`
twice back to back, then `cargo bench -p geode-core --bench scope --
expr_evaluate_row`. Apple M5 Pro, rustc 1.96.0, bench profile, 100 samples.
Load average (1 min) 16.7 → 9.3 → 6.0 across the runs, falling from 50–60
over five and fifteen minutes (other sessions' builds had just finished).

| Benchmark | Run 1 | Run 2 |
|---|---|---|
| `grid_build_1000` (unscoped, reference) | 1.65 ms (1.6499; 1.6461–1.6534) | 1.67 ms (1.6671; 1.6642–1.6699) |
| `apply_scope_1000` | 1.93 ms (1.9317; 1.9245–1.9383) | 1.92 ms (1.9165; 1.9066–1.9267) |
| `grid_build_1000_scoped` | 684 µs (684.21; 682.62–685.76) | 693 µs (692.62; 689.92–697.02) |
| `scope/expr_evaluate_row` (core) | 570 ns (570.10; 567.18–572.76) | — |

A scoped rebuild is `apply_scope` plus the scoped build: about 2.6 ms, inside
the 8 ms budget. `apply_scope` costs about 1.7 µs per line against the
evaluator's 0.57 µs per row: the rest is `SheetRow` reading each column the
way its cell paints it (`cell_text` formats the nine textual columns the text
filter searches). The scoped build is cheaper than the unscoped one because
it prepares half the rows.

**Bind once (2026-09-28).** `apply_scope` now binds the scope once
(`Scope::bind`: row-independent refusals, constants, the textual column list
and the lower-cased needle) and evaluates each line through `BoundScope`;
`expiry` reads an ISO date instead of the painted month code. Same fixture and
command, same machine, load average (1 min) 6.4 → 5.8:

| Benchmark | Run 1 | Run 2 |
|---|---|---|
| `grid_build_1000` (unscoped, reference) | 1.70 ms (1.6964; 1.6934–1.6994) | 1.71 ms (1.7092; 1.7034–1.7165) |
| `apply_scope_1000` | 891 µs (891.37; 886.30–896.26) | 910 µs (909.50; 896.42–927.02) |
| `grid_build_1000_scoped` | 702 µs (702.38; 696.72–706.59) | 701 µs (700.86; 691.28–712.98) |
| `scope/expr_evaluate_row` (core) | 561 ns (561.40; 558.85–563.80) | — |

`apply_scope_1000` roughly halves (1.93 → 0.90 ms, about 0.9 µs per line): the
per-row bind, `textual` list and needle lower-casing were the bulk of the gap
to the evaluator. A scoped rebuild is now about 1.6 ms.

Earlier runs of the same benches during a load spike are not comparable and
are recorded only as a warning: at load 34–43 `apply_scope_1000` read
5.67 ms and the scoped build 806 µs; at load 34 → 129 they read 11.3 ms and
18.5 ms with intervals several milliseconds wide.

### Pricer grouping (2026-09-28)

What a rebuild costs the pricer tile now that every grid is built from a
rollup tree. The tile's one build site runs the scope, `effective_chain`,
`rollup::build` and `GridModel::build`; an ungrouped sheet builds the
empty-chain rollup too, so its real flat rebuild is `rebuild_1000_flat`,
not `grid_build_1000` (kept as the grid-only reference with its history).

Fixtures: `grid_build_1000` / `rebuild_1000_flat` use the scope section's
sheet (1,000 entries, 1,200 rows, every package open, every line priced).
The grouped benches use 1,000 priced entries over SPX/NDX/SX5E/RTY ×
Z26/H27/M27, a call spread every tenth entry and a Z26/H27 calendar every
twentieth (so calendars split under `expiry`), under `[underlying_ref,
expiry, position_ref]` with every group and package open and the bundled
`vanilla` view: `rollup_1000` is the tree alone, `grid_build_1000_grouped`
the grid from a prebuilt tree (group rows fold and read unanimity over
their legs), `rebuild_1000_grouped` both, as the tile runs them.

`cargo bench -p geode-pricer --bench core --
'rollup_1000|grid_build_1000|rebuild_1000'` twice back to back. Apple M5
Pro, rustc 1.96.0, bench profile, 100 samples. Load average (1 min) 9.0 →
8.2 → 6.7 across the runs (fifteen-minute average about 17: other
sessions' builds).

| Benchmark | Run 1 | Run 2 |
|---|---|---|
| `grid_build_1000` (grid only, reference) | 1.96 ms (1.9582; 1.8700–2.0564) | 1.65 ms (1.6517; 1.6433–1.6617) |
| `rebuild_1000_flat` (empty-chain rollup + grid) | 1.68 ms (1.6825; 1.6789–1.6862) | 1.68 ms (1.6802; 1.6745–1.6874) |
| `grid_build_1000_scoped` | 681 µs (681.45; 679.96–683.00) | 685 µs (685.13; 683.18–687.54) |
| `rollup_1000` | 334 µs (333.72; 330.74–337.53) | 335 µs (335.05; 333.83–336.29) |
| `grid_build_1000_grouped` | 2.83 ms (2.8329; 2.8191–2.8519) | 2.83 ms (2.8306; 2.8099–2.8600) |
| `rebuild_1000_grouped` (rollup + grid) | 3.17 ms (3.1653; 3.1520–3.1820) | 3.15 ms (3.1453; 3.1390–3.1518) |

Run 1's `grid_build_1000` was the first bench after the build and its
interval is wide; run 2 matches the scope section's 1.65–1.70 ms. The
empty-chain rollup adds about 30 µs to a flat rebuild. A grouped rebuild is
about 3.2 ms, inside the 8 ms budget: the group rows' folds and unanimity
reads cost about 1.2 ms over the flat grid, the tree 0.33 ms. The tile's
cursor and anchor lookups (`row_at`, `painted_ancestor`, `Rollup::parent`)
are linear scans run once per rebuild and are not benched separately.

## Timeseries chart (spec §8, Part 3)

What one **cache miss** costs the render thread in `geode-chart`: the
two halves of turning a slot's values into a painted polyline —
min-max decimation (`core::decimate`), and decimation plus the path
rebuild through `gpui::PathBuilder`, which is lyon tessellating the
decimated points into the vertex buffer a frame submits. Spec §8.4
names the shape and the budget: 500,000 points — the series query's own
point cap — decimated into 1,600 columns, the width of a maximised plot
on a 4K screen, with a `NaN` hole every 5,000 points so the polyline
breaks like a real one. Under 2 ms for the second is the contract.

A cache **hit** costs neither. Both halves sit behind gpui-component's
`PathCache`, keyed on `(model.version, slot.number, pane, view)` plus
the pane's plot rect, so a repaint of an unchanged chart never runs
either of them (`geode_chart::rebuilds()` is the counter a test reads).
A hit is not free, though: `Window::paint_path` takes its path BY
VALUE, so `PathCache::get` clones the cached path and walks its
vertices to translate it on every call, hit or miss — O(the DECIMATED
points), about 3,200 per slot at the shape below, against the 500,000
the miss reads. The numbers in the table are the price of the frame
after a pan, a zoom, a resize or a delivery — once per visible slot —
and nothing else.

**Bench** (`cargo bench -p geode-chart`, criterion medians, the `bench`
profile — `--release` plus debug symbols — on an Apple M5 Pro):

| Benchmark | Result |
|---|---|
| `decimate/500k_into_1600` (decimation alone, into a reused buffer) | 847 µs |
| `decimate_and_path/500k_into_1600` (the same decimation plus the `PathBuilder` rebuild of its ~3,200 points) | 1.51 ms |

**§8.4's 2 ms holds**, at 1.51 ms — but with under half a millisecond
of headroom, and criterion's own upper bound for that row is 1.58 ms.
Two thirds of the budget is decimation, which is a linear scan of half
a million `f64`s; the tessellation of what comes out of it is the
cheaper half (about 660 µs). The honest reading is that the miss is
inside budget at the widest shape the spec names and is not comfortable
there: a slot near the cap that misses the cache on every frame of a
drag would spend most of a 60 Hz frame in this path, and three of them
would not fit. What makes it safe in practice is the cache, not the
margin — a pan invalidates the key once per gesture step, not per
slot-per-frame — and the first lever if it ever bites is the visible
slice (the element already decimates only `visible`, so a zoomed chart
pays for a fraction of the 500,000).

**What is NOT measured here: a painted frame.** The implementation
sandbox has no window, so every number above is CPU work measured
headless — `PathBuilder::build` tessellates without a GPU, which is why
the second row is measurable at all. The cost of submitting those
vertices, of the quads and the labels, and of the whole element's
`prepaint`/`paint` is unmeasured and belongs to the display check
(`cargo run -p geode-chart --example chart`, the example kept for
exactly that).

**The per-frame allocation exception.** PHILOSOPHY's "per-frame heap
churn is a defect" is met on the REBUILD — the `xs` buffer and the
decimated points are element-owned and reused, the paths and the chrome
derivation are cached, and a frame that changed nothing moves neither
`rebuilds()` nor `chrome_rebuilds()` — but not on the frame's own
submission, where two classes allocate every time and both are the
pinned API's price rather than a choice. First, the path clone above:
one vertex `Vec` per painted path per frame, bounded by the decimated
point count. Second, the chrome the component's own painters take:
`Grid` takes its lines as `Vec`s and `PlotAxis`/`PlotLabel` each
collect a small `Vec`, bounded by the tick count (tens). Neither is
O(the data) and neither is forked; what IS O(the data) — the
decimation, the tessellation, the side scales, the ticks and their
formatted labels — is what the two caches keep off an unchanged frame.

**Density bars are bounded, not cached.** A bar is one `paint_quad`
with nothing behind it, and the 2026-08-29 rendering spike disqualified
per-cell `paint_quad` past about 5,000 quads (10,000 cost 42 ms). The
model bounds nothing — `MAX_BINS` is 200 and a tile may hold many
slots — so the element counts the bars it paints across both panes and
stops at `MAX_DENSITY_QUADS` = 2,000 per frame, in slot order
(`geode_chart::timeseries::density_quads()` is the counter;
`a_frame_paints_at_most_the_density_bound` pins it). Unmeasured on a
real window, like everything else painted here.

## Timeseries module (spec §9, Part 4)

What one **delivery** costs the UI thread in `geode-timeseries`:
`core::chart::build` turning a `SeriesResult` into the `ChartModel` the
element paints from. The shape is the series query's own point cap —
500,000 buckets over four slots — and the work is copying: the bucket
vector once, each slot's `values` once, plus each slot's percentiles,
bins and prepared label. The tile then wraps the result in one
`Arc::new` and swaps it in (`rebuild_chrome`), which the bench does not
include and which costs an allocation, not a copy.

**Bench** (`cargo bench -p geode-timeseries`, criterion median, the
`bench` profile — `--release` plus debug symbols — on an Apple M5 Pro):

| Benchmark | Result |
|---|---|
| `chart_model/500k_x_4` (one delivery's model build at the point cap) | 259 µs |

That is five half-million-element vector clones — about 20 MB moved —
in a quarter of a millisecond, comfortably inside §7.1's 8 ms for a
pure-UI action and paid once per delivery, not per frame. (Task 3
recorded ≈397 µs for the same row on a warmer, busier machine; the two
readings bracket the cost rather than contradict each other.)

**What does NOT pay it.** Since Task 6's review the model is rebuilt
only when a field `chart::build` actually READS has moved, compared
through `ChartKey` (`tile.rs`): the result's own sequence number, each
slot's number/colour/axis/visibility/text, the frequency, the axis
mode, the split, whether density is on, the default source, the 28-value
theme signature and the named-colours `Arc` address. Everything else
that reaches `rebuild_chrome` — a cursor move (`tab`), a chip click, a
slot's state going `Fetching → Idle` when a fetch answers, a
`set_visible` on the tile — re-prepares the header and the title (one
`Chip` per slot, bounded by the slot count, never by the point count)
and then stops at the key compare: no value vector is copied, and —
just as importantly — `ChartModel::version` is not bumped, so every
path `geode-chart` has cached survives the frame
(`only_a_change_the_chart_model_reads_rebuilds_it` pins both halves).

A **view move** — a pan, a zoom, a jump — rebuilds neither the header
nor the chart model: the element takes `model.view()` beside the model,
so nothing here is copied and `version` does not move (`view_moved` is
a tail of its own, never `apply_changed`; the module doc calls it one
of the three). The element does pay: both of `geode-chart`'s cache keys
carry `view.key()`, so the frame after `h`/`l` re-decimates and
re-derives its chrome — the cache MISS Part 3 measured at 1.51 ms for
500,000 points into 1,600 columns, which is the budget §8.4 sets and
the reason the tile must not add a model rebuild on top of it. A view
move can also cost a REQUERY, and only while percentiles or density are
on, since both are computed over the visible window (ruling 10).

**Two per-notify costs are known and accepted.** First, while the add
picker's identities stage is open, every `Diagnostics` notification —
a source's health ticks about twice a second with a diagnostics tile
open — rebuilds and sorts the catalogue's option strings before
comparing them with the ones the list already holds; the comparison is
what keeps a trader's highlight still, but the rebuild happens either
way. It is bounded by the catalogue size and only while that stage is
up. Second, showing a hidden tile sends two queries: the show requeries
at once when it already has a result (hiding cleared `acted`, so
`follows_changed` says yes), and the fetch it sends alongside
answers `SeriesFetched Ok` a moment later, which requeries again. The
query pool coalesces by key — one in flight, the newer tag supersedes —
so the second question replaces the first rather than doubling the
work.

**Nothing painted here is measured.** The chips, the popups, the
expression strip, the range popup and the chart inside a real tile are
display-check items; the sandbox has no window.


### Publication request fan-out (2026-09-21)

The deterministic `publication_bursts_query_only_base_and_join_consumers` test drives two visible blotters through 128 publication notifications: 32 each for one view's base, its joined dataset, the other view's base, and an unrelated dataset. Targeted watches produce **96 query submissions (64 + 32), versus 256 under global invalidation**, a 62.5% reduction. The document-panel test produces zero document queries for 128 unrelated dataset/key publications, then one for the selected document. These are request counts through production tile handlers with a test data handle, not SQL execution timings or end-to-end frame latency measurements. Global frame observer dispatch and diagnostic catalog refresh remain outside this optimization.


### Diagnostics catalog request bound (2026-09-21)

`catalog_bursts_keep_one_read_and_one_follow_up` drives the production bridge with a test data handle: one visibility request, then 128 publications, draining UI notifications after each publication while withholding the first catalog reply. The bridge emits **two catalog requests total** (initial plus one follow-up), versus **129 attempted submissions** under the former per-notification behavior. With the initial request removed from the wire and its reply withheld, the old path fills the 64-slot request queue with catalog reads and refuses the remaining 64 submissions. The new path leaves **zero additional catalog requests ahead of an ordinary control request**, which is accepted immediately; the already-running catalog read can still delay service-thread work. Subsequent portions of the test exercise another publication during the follow-up and an unchanged completion.

These are deterministic submission/backlog counts, not SQL execution or elapsed query latency measurements. Catalog construction remains synchronous on the service thread, and sustained publications can keep one read active continuously. The change bounds amplification and outstanding work; it does not cap the cost of a single catalog read.


### Timeseries colour picker: one slider step (2026-09-26)

A slider drag in the colour picker commits on every step, and each commit
takes the `:colour` path: `Model::set_colour` then `apply_changed`, then
`rebuild_chrome`. A colour is resolved into the chart model, so each step
builds a new `ChartModel` and bumps its `version`, which clears
`geode-chart`'s path and chrome caches. The next frame then re-decimates
every slot.

Measured with a throwaway `#[gpui::test]` in release (`cargo test --release`,
removed after the run), on Apple M5 Pro with rustc 1.96.0. The fixture is the
crate's tile harness, headless: four visible source slots, one delivered
`SeriesResult` of `n` hourly buckets per slot, and the picker open on slot 4.
Each step is one `ColorPickerState::update_color`, which is what a slider step
calls. `VisualTestContext::update` draws the dirtied window before it returns,
so the "step" figure includes the component's own update, the tile's
rebuild, and one headless frame after the cache flush. That frame is not a
real GPU frame. Median of 30 steps:

| n (buckets × 4 slots) | step (update + frame) | tile tail alone | frame with a notify and no version bump |
|---|---:|---:|---:|
| 365 | 2.81 ms | — | 2.40 ms |
| 8,760 | 5.09 ms | — | 3.10 ms |
| 100,000 | 15.3 ms | — | 5.16 ms |
| 500,000 | 51.2 ms | 0.33 ms | 4.10 ms |

`chart::build` alone at 500,000 × 4 is 0.28 ms, in line with the 259 µs
Criterion reference. The tile's whole `set_colour` + `apply_changed` tail is
0.33 ms. The step's cost is the first frame after the `version` bump:
re-decimating four 500,000-point slots with the path cache cleared, about
47 ms of the 51 ms. At the daily and hourly sizes the demo uses, a step is
well inside the 8 ms budget. At 100,000 points or more it is over budget for a
live drag. This is a colour-only change invalidating cached geometry. It is
not a cost of the picker itself.

### Scope expression suggestions: ranking 20,000 values (2026-09-26)

Every keystroke in an open scope expression field re-lexes the field's text,
classifies the caret position, and — at a `Value` position — ranks that
column's cached distinct values with the shared fuzzy matcher, capping the
result at 50 rows. Columns and operators are always small sets; a categorical
column can be large, so `bench_expr_complete`
(`crates/geode-shell/benches/shell_cores.rs`) measures the worst realistic
case: a `refresh` at a value position already holding 20,000 ranked values.

Measured on Apple M5 Pro, rustc 1.96.0, `cargo bench -p geode-shell --bench
shell_cores -- expr_complete` (criterion, 30 samples):

| Benchmark | Median |
|---|---|
| `expr_complete/refresh_20k_values` | **6.82 ms** (range 6.78–6.87 ms) |

This includes one lex of the field's (short) text, `context_at`'s grammar
replay, and ranking 20,000 already-cached values down to 50 rows. It excludes
paint: the row list itself only ever renders the capped 50, and the distinct
query that first populated the 20,000 values is a separate, one-per-column
async round trip, not part of this cost. At 6.82 ms the reading sits under
the 8 ms pure-UI budget, but with little headroom — a key column can hold far
more than 20,000 distinct values, which is exactly why the picker restricts
value lists to categorical (dictionary-bounded) columns and never offers one
for a key.

### Grid selection: footer summary (2026-09-26)

`geode_blotter::core::select::summarize` builds the footer aggregate strip
(grid selection spec §3.3/§4.3): per selected measure column, an
`Accumulator` pass over the selection's top-most rows only, after a
`top_most` ancestor walk that drops any row whose parent is also selected.
Benched at the same 720,881-row, fully expanded shape
(`shape(80, 10, 900, 6)`) the `restore_by_path` benches above use, with a
`Rows` selection spanning the whole grid (`V` then `G` from the top) and
every one of the plan's measure columns — the worst case for both the
ancestor walk (every row present, so the walk actually needs to run) and the
accumulator loop (every measure column summed).

Measured with `cargo bench -p geode-blotter --bench blotter -- summarize`
(bench profile, `cargo bench`'s own defaults, no shortened sample window),
on Apple M5 Pro (MacBook Pro) with rustc 1.96.0:

| bench | median |
| --- | ---: |
| `summarize/720881 rows, all columns` | **1.2029 ms** |

Well inside the 8 ms UI-action budget with headroom to spare, so a
`V`-then-`G` top-of-grid selection over the demo's largest shape does not
need the tile to summarize lazily or off the UI thread.

## Ungrouped dimension columns (unanimity rule)

`cargo bench -p geode-data --bench query -- query_carried`: the `tree` view
(`lhu > underlying_ref > position_ref`, five measures over three grains)
against `tree_carried`, the same view plus ungrouped `strike` (f64) and
`expiry` (utf8), both carried at instrument grain. One service over one
million generated rows (959,012 ingested) with both columns stored, so the
two views read identical tables and differ only in the statement. Their
aggregates fold into the underlying-grain measure scan; result row counts
are identical (136,868 scoped, 133 scoped at depth 2, 729,466 unscoped).

Conditions: Apple M5 Pro (18 cores, 48 GB), rustc 1.96.0, bench profile,
20 samples. **The machine was heavily loaded** (load average 34–43 from
concurrent builds in other checkouts), so absolute values are inflated —
the plain `tree` rows here are several times the Phase 2b table above. Read
the pairs as a same-run ratio, not as reference values.

| shape | `tree` median | `tree_carried` median | ratio |
| --- | ---: | ---: | ---: |
| scoped to 3 books, depth 2 | 21.8 ms | 32.3 ms | 1.48 |
| unscoped, depth 2 | 25.9 ms | 41.2 ms | 1.59 |
| scoped to 3 books, full depth | 124 ms | 184 ms | 1.48 |
| unscoped, full depth | 600 ms | 735 ms | 1.22 |

The depth-2 shapes the blotter opens with stay under the 50 ms requery
budget with both columns even under this load. The added cost is the two
`min`/`max`/`count` pairs per underlying row and one numeric, one text, and
two boolean columns across the Arrow boundary. An unloaded re-measure is still
owed before these become reference values.

## Market-data panels as owned specs

`cargo bench -p geode-marketdata --bench matrix`, before and after the
panel family became owned (`String`/`Vec`, choices behind an `Arc`) and
tiles began holding `Arc<PanelSpec>`. A build clones each slice and flat
label once per column, never per row; choices are an `Arc` clone. Nothing
on the per-row paths changed: row labels, cells and key extraction borrow
the spec's strings.

Conditions: Apple M5 Pro (18 cores), rustc 1.96.0, bench profile. **The
machine was saturated** by concurrent builds in other checkouts (load
average 36 during the baseline, 97–185 during the comparison), so absolute
values are inflated and swing several-fold between identical runs. To
compare like with like, the pre-change bench binary was kept and run
alternately with the post-change one, three rounds; each cell is a
Criterion median.

| bench | before r1 | after r1 | before r2 | after r2 | before r3 | after r3 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| model_build_pivot_20x30 | 8.66 ms | 4.47 ms | 1.94 ms | 1.42 ms | 5.24 ms | 5.54 ms |
| model_build_values_10000x5 | 226 ms | 55.5 ms | 108 ms | 26.0 ms | 190 ms | 111 ms |
| patch_cell_values_10000x5 | 2.98 µs | 389 ns | 2.10 µs | 2.26 µs | 2.64 µs | 667 ns |

The first baseline (`--save-baseline panels-before`, load 36) read
791 µs / 21.8 ms / 474 ns for the same three benches. No pair shows the
post-change binary slower beyond the run-to-run swing; the load rules out a
10 % bound either way.

Re-measured with the same two binaries in alternation once the machine was
quieter (one-minute load average 2–7 for the first two rounds; the later
rounds rose to 35 again and are left out). Criterion medians:

| bench | before r1 | after r1 | before r2 | after r2 |
| --- | ---: | ---: | ---: | ---: |
| model_build_pivot_20x30 | 312 µs | 316 µs | 317 µs | 314 µs |
| model_build_values_10000x5 | 8.15 ms | 8.17 ms | 8.45 ms | 8.06 ms |
| patch_cell_values_10000x5 | 171 ns | 161 ns | 170 ns | 165 ns |
| draft_rebase_1000_edits | 3.23 ms | 3.26 ms | 3.08 ms | 3.33 ms |

Builds and patches are unchanged within run-to-run noise. The draft rebase
reads 1–8 % slower after in every round, loaded or not; at 3.3 ms for a
thousand edits it stays well inside the 8 ms budget. The flat 10,000-row
build sits at the budget boundary before and after alike, as
`docs/current/performance.md` already records. Both binaries read above that
guide's reference values on this day, so the reference values are left
as they are.

## Diagnostics page: the Log rebuild over a full tail (headless)

The page rebuilds the selected section when that section's inputs change
(`docs/current/features.md#diagnostics`); the Log section's rebuild is the
widest, because it walks the whole retained tail. This reading times
`DiagnosticsPage::rebuild` for the Log section with the tail full, from a
`TestAppContext` window with no display: **a headless rebuild, not a
painted frame**. Recipe:

```sh
cargo test -p geode-diagnostics --release -- --ignored log_rebuild_timing --nocapture
```

The ignored test (`page::tests::log_rebuild_timing_over_a_full_tail`) opens
the page on Log over an 8,192-slot ring, pushes 4,096 records (one in five
an error, one in five a warning, one in five debug, the rest info, each with
a 50-character message), drains them with one notify, then times twenty
further `rebuild` calls and prints the median and the maximum.

Conditions: Apple M5 Pro (18 cores, 48 GB), macOS 26.4, rustc 1.96.0,
2026-09-27, an otherwise idle machine; twenty rebuilds after the first
drain.

| build | median rebuild | max rebuild |
| --- | ---: | ---: |
| debug (`cargo test`, unoptimized) | 28.1 ms | 30.6 ms |
| release (`--release`) | 4.65 ms | 7.23 ms |

What a rebuild here includes: the drain (a no-op after the first), the
4,096 record clones into typed rows with a clock-formatted timestamp each,
the 4,096 prepared rows with their detail line, the table's `set` and
`refresh`, the target-select item comparison, the badge pass over the tail
(the error count), and the header chips. It runs once per drain that
brings records while Log is shown, never per frame, and the tail is capped
at 4,096 so it cannot grow past this shape. In release it sits inside the
8 ms pure-UI budget, with the maximum close to it; a tail of long messages
or a slower machine would take it over, in which case the next step is to
stop rebuilding the whole prepared table on an append (retain the prepared
rows and push only the drained ones), not to widen the budget.

What it does not measure: the paint of the visible rows (the table
virtualises), the rail and header, and the frame-time p95 with the page
open beside a blotter under a held `j`. That painted reading, taken from
the perf overlay with the counters reset before the hold, stays on the
display-check list for this branch.

## 2026-09-29 — Fzf typing over 1.5 million rows

Measured on the local Apple Silicon macOS development workstation with
`rustc 1.96.0 (ac68faa20 2026-05-25)`. A standalone `rustc --edition=2024 -O`
harness compiled the actual `SearchText`, `rank_index`, and palette matcher
functions extracted from the source before and after this change; the updated
harness also compiled `fuzzyfind/scoring.rs`. This isolates ranking without
rebuilding or timing the full GUI application. Hardware model was unavailable
inside the measurement sandbox.

Fixture: 1,500,000 labels `Contract {i:07}`, each with ancestor path
`Equities › US › Book A`, normalized once before timing. Queries run in the
order shown below. The new harness retains the previous completed result set
and uses it only when `can_narrow` permits it. The old harness scans the whole
index for each query, as the previous implementation did. Timing covers matching,
sorting, and collecting results; the updated harness also drops the previous
compact result vector when replacing it. Index preparation, input events,
visible-row highlighting, layout, and paint are excluded.

| Query | Original, one run | Updated, range of three runs | Matches |
| --- | ---: | ---: | ---: |
| `c` | 394.7 ms | 15.0–16.3 ms | 1,500,000 |
| `co` | 512.5 ms | 16.7–17.3 ms | 1,500,000 |
| `contract` | 1,260.9 ms | 17.7–19.7 ms | 1,500,000 |
| `149` | 480.9 ms | 58.0–60.0 ms | 89,385 |
| `1499` | 512.9 ms | 8.5–9.0 ms | 12,840 |
| `14999` | not measured | 1.3–1.5 ms | 1,095 |
| `contract 1499` | 1,816.6 ms | 74.5–75.2 ms | 12,840 |

The initial rolling-score implementation, before the consecutive-prefix fast
path, still took 816 ms for `contract`; the prefix bound brought that to about
19 ms without changing scores. Differential tests compare reused scorer
storage against the palette matcher on exhaustive short ASCII/Unicode
candidates, separators, overlapping words, and ancestor paths. Ranking tests
also cover source-order ties, candidate narrowing, Unicode highlight offsets,
and cancellation. UI tests cover backspace, dataset replacement, keyboard
selection, and native-table scrolling.

The permanent ignored test `fuzzyfind::tests::fzf_rank_large_index` retains the
fixture and query sequence for future optimized diagnostic runs. These are
synthetic ranking measurements, not end-to-end typing latency. Unselective
multi-word queries still use the original scorer for surviving candidates;
the numbers above do not establish a bound for every query or dataset.

## 2026-09-29 — Display parents and leaves before indexing

Replaced the 64-row loaded-tree preview with a deferred label source containing
the complete loaded row order. The immutable snapshot prepares and shares its
depth-first order on the query worker; local sorting uses the full table order
when available and otherwise computes the sorted full order. Visible labels
are generated on demand. Background indexing replaces only the search data,
preserving the initial empty-query order.

`CARGO_CACHE_RUSTC_INFO=0 GEODE_FZF_TEST_ROWS=1500000 cargo test -p geode-blotter
--lib fzf_open_large_snapshot -- --nocapture` on the same workstation/toolchain,
unoptimized development profile:

| Descendants | Open | First paint | Full index ready | Cached reopen |
| ---: | ---: | ---: | ---: | ---: |
| 100,000 | 31.0 ms | 59.5 ms | 360.6 ms | 26.6 ms |
| 1,500,000 | 43.5 ms | 72.0 ms | 3,927.3 ms | 26.6 ms |

Fixture: grand total, one `L1` parent, and numbered `Contract` descendants,
initially collapsed, with one numeric column and no local sort. Preparation of
the snapshot and tree index is excluded. Open/paint include the headless GPUI
table. Assertions navigate to both the parent and the final descendant before
running the search worker, then verify search and cache reuse afterward.

Earlier 100k measurements of roughly 3 ms open / 5 ms first paint displayed
only a preview and preceded the fix to reuse original column header renderers;
they are not directly comparable full-table measurements. The new behavior
eliminates the delayed insertion of parents instead of waiting for all search
text to normalize before displaying them. This is not a release-app benchmark,
and does not include queries needed to load missing descendants or the cost of
a local sort over collapsed groups.

## 2026-09-29 — Ranked tree, line numbers, and search-only folds

The selected B presentation retains direct matches and each ancestor once,
ranking sibling branches by their strongest match. Direct-match candidates are
kept separately from context rows and folded rows, preserving query narrowing
and match counts. The worker links siblings in discovery order and emits DFS;
it does not sort the hierarchy a second time. Native table cells retain their
headers, indentation, numeric formatting, line-number setting, and chevrons.

Same workstation/toolchain and optimized standalone extraction method as above,
now including the actual `FindTree` implementation. Fixture adds `Total` and
`Book A` above the 1,500,000 contract leaves; queries and narrowing are unchanged.
One run, with index and topology constructed before timing:

| Query | Matching and ranking | Including hierarchy ordering | Direct matches |
| --- | ---: | ---: | ---: |
| `c` | 17.2 ms | 27.2 ms | 1,500,000 |
| `co` | 19.4 ms | 27.1 ms | 1,500,000 |
| `contract` | 19.6 ms | 27.2 ms | 1,500,000 |
| `149` | 57.7 ms | 58.7 ms | 89,385 |
| `1499` | 8.5 ms | 8.9 ms | 12,840 |
| `contract 1499` | 75.0 ms | 75.4 ms | 12,840 |

An earlier sibling-sort/hash-lookup implementation took 70–73 ms including
hierarchy for the broad prefixes; replacing that pass with links removed most
of the added cost. These synthetic measurements exclude input dispatch,
visible highlighting, layout, and paint. They do not bound arbitrary multi-word
queries or different tree shapes.

The same 1.5m headless development-profile opening test measured 52.5 ms open,
85.0 ms first paint, 4.064 s index completion, and 28.7 ms cached reopen. Building
search topology synchronously had regressed first paint to 164 ms; it now runs
on the indexing worker, while visible chevrons consult the snapshot's existing
tree. Every loaded parent and leaf remains available at first paint. A fold
made before indexing finishes remains folded during and after replacement.

## 2026-09-29 — Search continuity and cold-open allocation

A geometry regression test with long group labels reproduced the search text
starting 5 px farther right than the expanded normal tree. The search renderer
had a fixed disclosure slot and a separate truncating label container, while
the normal table let the slot shrink alongside its direct text child. Search
now follows the normal layout, including left-aligned disclosure glyphs, leaf
dots, and the gutter/disclosure on the blank grand-total row. Tests compare
resolved cell bounds and disclosure edges with numbers Off, On, and Relative.

Cold-open phase instrumentation on the same 1.5m fixture found 13.7 ms spent
preparing the display, mostly constructing the identity row permutation. The
empty-query order now stores a row count and resolves positions directly;
ranked results still share their explicit row vectors. This eliminates the
12 MB identity vector at 1.5m rows and its UI-thread construction, without
waiting for background indexing or displaying only a preview.

Same unoptimized headless fixture and command as above, one run:

| Descendants | Open | First paint | Full index ready | Cached reopen |
| ---: | ---: | ---: | ---: | ---: |
| 1,500,000 | 34.5 ms | 67.8 ms | 3,965.3 ms | 31.2 ms |

The preceding ranked-tree measurement was 52.5 ms open / 85.0 ms first paint;
the pre-ranked-tree measurement was 43.5 / 72.0 ms. These are development-build
headless timings, not end-to-end release-app latency. The label index still
builds on the worker after all loaded parents and leaves are displayed.

## Diagnostics result summaries — 2026-09-30

Apple M5 Pro (aarch64 macOS), Rust 1.96.0, release profile. The existing
`log_rebuild_timing_over_a_full_tail` headless test rebuilds 4,096 retained
records with mixed levels and representative messages, twenty times per run.
It includes model preparation, filtering, table replacement, and chrome
refresh; it excludes real-window layout, text painting, and GPU work.

Command: `cargo test -p geode-diagnostics --release -- --ignored log_rebuild_timing --nocapture`.

| Version | Median | Maximum |
|---|---:|---:|
| Before result counts and row-position readouts | 5.229 ms | 5.791 ms |
| With result counts and row-position readouts | 5.057 ms | 6.228 ms |

These single-run values show no material median regression; the difference is
not evidence of a speedup. Performance-section percentile labels, histogram
geometry, and tooltip strings are prepared on rebuild, outside render. That
section is not included in this log-tail measurement.

## Diagnostics Data leaf filtering — 2026-09-30

Apple M5 Pro (aarch64 macOS), Rust 1.96.0, release profile. The pure
`data_filter_timing_over_a_catalog` fixture has 20 datasets with 200 generation
rows each. Each case runs twenty times. This measures `prepared::data_table`
only, excluding catalog-model construction, GPUI updates, layout, and paint.

Command: `cargo test -p geode-diagnostics --release data_filter_timing_over_a_catalog -- --ignored --nocapture`.

| Query | Visible rows | Before median / maximum | After median / maximum |
|---|---:|---:|---:|
| Empty | 4,020 | 0.971 / 1.733 ms | 0.943 / 1.642 ms |
| Dataset name (`dataset`) | 4,020 | 0.815 / 0.894 ms | 0.789 / 0.877 ms |
| Leaf label (`p100`) | 40 | Unsupported | 0.204 / 0.237 ms |

The leaf result includes 20 matching generations and their dataset headings.
The previous filter returned no rows for that query, so its timing is not a
valid comparison. The equivalent empty and dataset-name cases show no material
regression; these single runs do not establish a speedup.

## Windowed grid models: before — 2026-10-01

Apple M5 Pro, 18 cores, rustc 1.96.0 (ac68faa20 2026-05-25), bench profile.
Load average at start: 4.69 12.38 11.52 (the recorded run; one-minute load
2.79–4.79 throughout). Binaries kept as `bench-grid/{matrix,core,blotter}-before`
for alternating runs against the windowed build.

An earlier run of the same binaries began at load 18.09 21.70 13.72 (one-minute
load falling to 4.67 by its end, system indexing daemons busy); its medians are
given beside the recorded ones and agree within 2%.

`window_fill_*` before bodies read the same cells' prepared text (the per-frame
paint cost); `delivery_to_window_values_10000x5` and
`deliver_unchanged_structure_1000*` are the end-to-end comparisons.

| Benchmark | Before median | Earlier run (load 18 → 5) |
|---|---:|---:|
| `marketdata_core/model_build_pivot_20x30` | 207.89 µs | 210.61 µs |
| `marketdata_core/model_build_values_10000x5` | 5.4305 ms | 5.4841 ms |
| `marketdata_core/model_build_values_10000x5_100_rows_spliced` | 5.7479 ms | 5.8184 ms |
| `marketdata_core/window_fill_40x5` | 175.00 ns | 176.74 ns |
| `marketdata_core/delivery_to_window_values_10000x5` | 5.4664 ms | 5.4702 ms |
| `marketdata_core/one_cell_edit_values_10000x5` | 173.97 ns | 174.82 ns |
| `marketdata_core/session_tick_values_10000x5` | 6.4126 ms | 6.3766 ms |
| `marketdata_core/patch_cell_pivot_20x30` | 86.770 ns | 87.036 ns |
| `marketdata_core/patch_cell_values_10000x5` | 116.05 ns | 112.67 ns |
| `marketdata_core/draft_rebase_1000_edits` | 2.0607 ms | 2.0842 ms |
| `pricer_core/grid_build_1000` | 1.0502 ms | 1.0551 ms |
| `pricer_core/grid_build_1000_scoped` | 437.17 µs | 442.41 µs |
| `pricer_core/grid_build_1000_grouped` | 1.7773 ms | 1.7941 ms |
| `pricer_core/window_fill_40` | 502.76 ns | 498.64 ns |
| `pricer_core/window_fill_40_grouped` | 499.58 ns | 496.21 ns |
| `pricer_core/deliver_unchanged_structure_1000` | 1.2330 ms | 1.2501 ms |
| `pricer_core/deliver_unchanged_structure_1000_grouped` | 2.1642 ms | 2.1908 ms |
| `pricer_core/rebuild_1000_flat` | 1.1006 ms | 1.0907 ms |
| `pricer_core/rebuild_1000_grouped` | 2.0104 ms | 2.0376 ms |
| `blotter_core/cache_fill_40x7_133_rows` | 32.033 µs | 31.950 µs |
| `blotter_core/cache_fill_40x7_137k_rows` | 33.188 µs | 33.652 µs |
| `blotter_core/cache_fill_40x7_729k_rows` | 38.318 µs | 38.783 µs |

## Windowed grid models: after — 2026-10-01

Apple M5 Pro, 18 cores, rustc 1.96.0 (ac68faa20 2026-05-25), bench profile.
The before binaries (`bench-grid/{matrix,core,blotter}-before`) and the after
binaries were run alternately, before then after for each filter, over two
rounds. Both share one Criterion baseline, so the printed medians are
compared, not the "change:" lines.

- Round 1 began at load 9.61 5.12 4.25 (just after the bench build) and rose
  to 18.82 during the pricer pairs (system daemons); it ended at 9.49 9.51 6.79.
- Round 2 (recorded) began at 8.73 9.36 6.75; one-minute load fell to 4.53
  within the first pair and stayed 1.80–5.17; it ended at 1.80 3.53 4.75.

| Benchmark | Before | After | Round 1 (before / after) | What after measures |
|---|---:|---:|---:|---|
| `marketdata_core/model_build_values_10000x5` | 5.4128 ms | 586.01 µs | 5.4882 ms / 618.86 µs | `MatrixIndex::build`: labels and row facts, no cell text |
| `marketdata_core/model_build_values_10000x5_100_rows_spliced` | 5.7528 ms | 898.34 µs | 5.9231 ms / 934.81 µs | the same with 100 spliced rows |
| `marketdata_core/model_build_pivot_20x30` | 207.85 µs | 119.80 µs | 211.10 µs / 127.93 µs | the pivot index |
| `marketdata_core/window_fill_40x5` | 175.60 ns | 17.716 µs | 177.10 ns / 18.480 µs | a cold 40 × 5 window fill (before: cloning prepared text) |
| `marketdata_core/delivery_to_window_values_10000x5` | 5.3860 ms | 619.28 µs | 5.5953 ms / 645.80 µs | index build plus a 40 × 5 fill (before: whole build) |
| `marketdata_core/one_cell_edit_values_10000x5` | 169.47 ns | 161.88 ns | 179.20 ns / 168.61 ns | draft write plus one window refill (before: plus `patch_cell`) |
| `marketdata_core/patch_cell_values_10000x5` | 113.46 ns | 105.33 ns | 111.56 ns / 108.50 ns | one-cell window refill (before: `patch_cell`) |
| `marketdata_core/patch_cell_pivot_20x30` | 85.003 ns | 89.018 ns | 87.302 ns / 91.087 ns | one-cell window refill on the pivot |
| `marketdata_core/session_tick_values_10000x5` | 6.3419 ms | 945.18 µs | 6.5934 ms / 1.0089 ms | group capture from the installed index (before: a clean build first) |
| `marketdata_core/draft_rebase_1000_edits` | 2.0299 ms | 1.9641 ms | 2.1667 ms / 2.1085 ms | rebase reading `label_index` |
| `marketdata_core/draft_rebase_1000_edits_100_rows` | 2.0354 ms | 1.9640 ms | 2.1905 ms / 2.1267 ms | the same with 100 inserted rows |
| `pricer_core/grid_build_1000` | 1.0495 ms | 271.14 µs | 1.1340 ms / 291.35 µs | `GridIndex::build` |
| `pricer_core/grid_build_1000_scoped` | 438.19 µs | 113.75 µs | 439.55 µs / 116.01 µs | the scoped index |
| `pricer_core/grid_build_1000_grouped` | 1.7931 ms | 308.06 µs | 1.8104 ms / 326.80 µs | the grouped index |
| `pricer_core/rebuild_1000_flat` | 1.0782 ms | 343.39 µs | 1.2528 ms / 429.28 µs | empty-chain rollup plus the index, no window fill |
| `pricer_core/rebuild_1000_grouped` | 2.0130 ms | 609.78 µs | 3.9650 ms / 588.73 µs | rollup plus the grouped index, no window fill |
| `pricer_core/window_fill_40` | 500.13 ns | 29.176 µs | 498.75 ns / 37.125 µs | a cold 40-row fill |
| `pricer_core/window_fill_40_grouped` | 504.45 ns | 205.86 µs | 497.97 ns / 338.42 µs | a cold fill whose group rows fold their legs |
| `pricer_core/deliver_unchanged_structure_1000` | 1.2402 ms | 240.86 µs | 2.1372 ms / 327.78 µs | refill-only (before: the tile's whole rebuild) |
| `pricer_core/deliver_unchanged_structure_1000_grouped` | 2.1724 ms | 633.29 µs | 3.5607 ms / 849.31 µs | refill-only, grouped |
| `blotter_core/cache_fill_40x7_133_rows` | 37.086 µs | 33.922 µs | 32.185 µs / 30.896 µs | the shared `WindowCache` |
| `blotter_core/cache_fill_40x7_137k_rows` | 40.537 µs | 36.496 µs | 44.124 µs / 32.414 µs | the shared `WindowCache` |
| `blotter_core/cache_fill_40x7_729k_rows` | 47.560 µs | 45.219 µs | 44.653 µs / 37.202 µs | the shared `WindowCache` |

Every number is headless model work: no paint, no table layout, no text
shaping. The index builds exclude cell text entirely; the window fills
format only the cells they name, through the module's one formatter, into a
fresh cache. A tile's real rebuild is the index build plus one window fill
(market data 586 µs + 17.7 µs; pricer flat 343 µs + 29 µs, grouped 610 µs +
206 µs). The pricer's refill-only delivery still installs the answers,
re-applies the scope, re-derives the chain and rollup and compares the tree,
so it saves the index build, not the per-delivery scope and rollup work.

Named regressions. The `window_fill_*` rows rise by two orders of magnitude
because before they cloned text already prepared by the whole build and after
they format from cold; the cost moved from every build to the visible window
and is paid once per scroll or install, not per frame. The grouped pricer fill
(206 µs; 338 µs in the loaded round) is the largest: a group row sums and
reads unanimity over all its legs when the window reaches it. The pivot
one-cell refill is about 4 ns slower (85 → 89 ns), within the spread of the
round pair. No blotter `cache_fill` shape regressed: after was faster than
before in both rounds, by 1–12 µs, a spread comparable to the rounds' own
difference.

## Fuzzy `/` result table: visible rows only — 2026-10-01

Apple M5 Pro, 18 cores, rustc 1.96.0 (ac68faa20 2026-05-25), bench profile.
`marketdata_core/find_open_cells_values_10000x5` measures the cells a `/` open
on the 10,000 × five-value schedule prepares before its first frame paints.
Before, that was every row into a `WindowCache` (`0..10_000`, 50,000
`md_cell` calls). After, it is the find table's first window (rows `0..64`,
reported before the table measures its range) into a `RowCache`, then the 40
rows it shows, all already held (320 `md_cell` calls). The before binary
(`bench-find/matrix-before`, built from the same bench with the old body) and
the after binary were run alternately over two rounds, before then after,
then after then before. Both share one Criterion baseline, so the printed
medians are compared, not the "change:" lines.

The machine was heavily loaded by other sessions' builds: one-minute load
28.50 at the first run, 26.70, 22.83 and 21.16 between runs, 15.81 at the end
(37.81 during the very first, discarded, before run, which measured 7.48 ms).

| Run | Before | After |
|---|---:|---:|
| Round 1 (before, after) | 7.9808 ms | 52.678 µs |
| Round 2 (after, before) | 7.5263 ms | 53.439 µs |

Headless model work only: no table layout, no paint, no text shaping, and not
the `FindItem` construction or search-text build a `/` open also performs
(neither changed). The real first frame also formats the 64-row first window
before the table's first range report drops the rows it does not show; a
scroll then formats only the rows entering view. The pricer's open drops its
all-open `fill_all` the same way; it has no bench of its own (its `GridIndex`
build for `/`, tree labels only, is unchanged and `pricer_core/grid_build_1000`
covers it).

## Pricer column sort: sorted rebuilds — 2026-10-01

Apple M5 Pro, 18 cores, rustc 1.96.0 (ac68faa20 2026-05-25), bench profile.
`pricer_core/rebuild_1000_flat_sorted` and `rebuild_1000_grouped_sorted` run
the tile's structural rebuild under an `npv` descending sort: the rollup,
`core::sort::rank` over every sibling set (a group keyed by its legs' fold, a
package by its own folded result, a line by its result), then
`GridIndex::build`, before any window fill. The flat shape is
`texts(1_000)` (1,200 sheet rows, every package open); the grouped one
`grouped_texts(1_000)` under `[underlying_ref, expiry, position_ref]`, every
group and package open. Prices vary per line (`(row × 7919) mod 1000 − 500`)
so the ranks are not all ties. `rank_1000_grouped` is the ranking alone on a
fresh grouped rollup. The same commit also sorts `rollup::legs_under`'s
output into sheet order, which every unsorted grouped build now pays too.

The machine was heavily loaded by other sessions' builds: one-minute load
49.14 at the first run's start, 21.49 at the second's, 10.61 at its end. The
unsorted reference rows ran in the same invocations; compare within a round,
not against the reference table's idle figures (`grid_build_1000` read
473 µs and 425 µs here against its 271 µs reference).

| Bench | Round 1 | Round 2 |
|---|---:|---:|
| `grid_build_1000` | 473 µs | 425 µs |
| `rebuild_1000_flat` | 517 µs | 469 µs |
| `rebuild_1000_flat_sorted` | 561 µs | 549 µs |
| `rebuild_1000_grouped` | 1.32 ms | 834 µs |
| `rebuild_1000_grouped_sorted` | 1.04 ms | 923 µs |
| `rank_1000_grouped` | 108 µs | 102 µs |

Round 1's unsorted grouped figure (1.32 ms, a 1.15–1.54 ms interval) is load
noise: its sorted twin ran faster. In round 2 the sort adds about 80 µs flat
and 90 µs grouped, about 1% of the 8 ms pure-UI budget; the
ranking is bounded by the grouped fold (each group folds its legs once per
level). Headless model work only: no table layout or paint. A price-only
delivery under a sort runs the same rank before its structural comparison;
when the order is unchanged it still refills only (the tile test
`a_price_refresh_re_ranks_a_measure_sort_and_the_cursor_stays_on_its_line`
asserts no index build), and with no sort no rank runs.

## Prepared chrome models: before — 2026-10-02

Apple M5 Pro, 18 cores, rustc 1.96.0 (ac68faa20 2026-05-25), bench profile.
Two runs, because the first started above load 8. Run 1 load average at start:
12.43 5.36 3.12; at end: 5.85 4.87 3.18. Run 2 at start: 5.85 4.87 3.18; at
end: 4.01 4.57 3.26. Binary kept as `bench-chrome/shell_cores-before` for
alternating runs.

Fixtures. `keybindings_rows`: the shell's builtin actions plus 400 synthetic
module actions in eight contexts (507 actions in all), keymap of the builtin
layer plus a user layer of 200 rebinds and five `"none"` shadows; `geode-shell`
cannot depend on the composition root, so module registrations are synthetic.
`settings_rows`: every bundled theme (44), four fetch sources.
`object_browse_rows`: 500 views. `object_edit_rows`: the demo desk's view with
the most edit rows (`wide`, 111 rows). `palette_rows`: builtin plus 400 actions
and every bundled theme (551 items).

Each `derive_rank_*` is what one render paid before this slice, and what each
key and click handler paid again: rows derived and ranked from scratch.

| Benchmark | Before median, run 1 | Before median, run 2 |
|---|---:|---:|
| `keybindings_rows/derive_rank_empty` | 696.02 µs | 720.31 µs |
| `keybindings_rows/derive_rank_typed` | 802.28 µs | 807.92 µs |
| `keybindings_rows/rank_typed` | 115.40 µs | 107.89 µs |
| `settings_rows/derive_rank_empty` | 2.267 µs | 2.266 µs |
| `settings_rows/derive_rank_typed` | 3.539 µs | 3.395 µs |
| `object_browse_rows/derive_rank_empty` | 192.05 µs | 190.15 µs |
| `object_browse_rows/derive_rank_typed` | 406.14 µs | 405.20 µs |
| `object_browse_rows/rank_typed` | 209.10 µs | 208.07 µs |
| `object_edit_rows/derive_rank_empty` | 1.855 µs | 1.836 µs |
| `object_edit_rows/derive_rank_typed` | 13.626 µs | 13.181 µs |
| `palette_rows/paint_rows_empty` | 38.890 µs | 39.335 µs |
| `palette_rows/paint_rows_typed` | 7.053 µs | 7.183 µs |

Headless model work only: no layout, paint or text shaping.

## Prepared chrome models: after — 2026-10-02

Apple M5 Pro, 18 cores, macOS 26.4, rustc 1.96.0 (ac68faa20 2026-05-25), bench
profile. The before binary (from the entry above) and the after binary were run
alternately, before, after, after, before, filtered to `_rows/`, sharing one
Criterion baseline directory: the printed medians are compared, never the
"change:" lines. Load average: before round 1 start 5.97 2.74 2.85 (a
transient; it fell to 2.43 by its end); after round 1 end 3.04 2.67 2.77;
after round 2 end 1.71 2.31 2.61; before round 2 end 1.67 2.08 2.48. Fixtures
as in the before entry; the keybinding registry is synthetic (builtin plus 400
module actions), because `geode-shell` cannot depend on the composition root.

| Benchmark | Before (round 1 / 2) | After (round 1 / 2) | What after measures |
|---|---:|---:|---|
| `keybindings_rows/derive_rank_empty` | 692.1 / 691.7 µs | 668.2 / 666.1 µs | one refresh after an input change (open, applied reload) |
| `keybindings_rows/derive_rank_typed` | 807.9 / 780.2 µs | 779.6 / 775.3 µs | the same with a query |
| `keybindings_rows/rank_typed` | 108.0 / 106.2 µs | 104.9 / 103.8 µs | ranking alone |
| `keybindings_rows/prepared_rerank` | — | 113.0 / 113.1 µs | one filter keystroke: `set_query` + re-rank, no derivation |
| `settings_rows/derive_rank_empty` | 2.20 / 2.22 µs | 2.23 / 2.22 µs | unchanged: settings still derive per render (dropped from the slice) |
| `settings_rows/derive_rank_typed` | 3.48 / 3.30 µs | 3.34 / 3.39 µs | unchanged |
| `object_browse_rows/derive_rank_empty` | 196.2 / 189.7 µs | 187.4 / 189.2 µs | one refresh after an input change |
| `object_browse_rows/derive_rank_typed` | 411.3 / 396.0 µs | 398.0 / 400.3 µs | the same with a query |
| `object_browse_rows/rank_typed` | 224.4 / 219.9 µs | 203.5 / 213.3 µs | ranking alone |
| `object_browse_rows/prepared_rerank` | — | 234.9 / 245.5 µs | one filter keystroke: `set_query` + re-rank, no derivation |
| `object_edit_rows/derive_rank_empty` | 1.79 / 1.79 µs | 1.83 / 1.89 µs | unchanged: the Edit stage still derives per render (dropped) |
| `object_edit_rows/derive_rank_typed` | 13.24 / 13.42 µs | 13.14 / 12.95 µs | unchanged |
| `palette_rows/paint_rows_empty` | 39.28 / 40.34 µs | 0.277 / 0.273 µs | one paint: the rows in view (`VISIBLE_ROWS`), prepared labels |
| `palette_rows/paint_rows_typed` | 7.30 / 6.97 µs | 0.790 / 0.775 µs | the same with a query |

Read per path, not per bench. Per repaint, before paid a `derive_rank_*`
(keybindings about 0.7 to 0.8 ms, object browse 0.19 to 0.41 ms) and the
palette paid `paint_rows_*` over every result; after, render reads the
prepared slices, deriving and ranking nothing, and the palette paints at most
`VISIBLE_ROWS` rows (0.27 to 0.79 µs, 50 to 140 times less). Per filter
keystroke, before paid `derive_rank_typed` at least twice (the handler and the
render): about 1.6 ms for keybindings and 0.8 ms for browse. After, one
`prepared_rerank`: 113 µs and 235 to 245 µs. The derive-and-rank cost is
unchanged per refresh and is now paid only when an input changes.

`object_browse_rows/prepared_rerank` is 20 to 30 µs above `rank_typed`: it also
builds the shown rows (`Shown`, with the highlight ranges) that ranking alone
does not. No regression otherwise; the settings and object-edit rows are within
run-to-run noise of before, as expected for paths this slice left alone.

Headless model work only: no layout, paint or text shaping.

## Diagnostics fuzzy filter (2026-10-02)

Apple M5 Pro, rustc 1.96.0, release/bench profiles. The machine was heavily
loaded throughout (load average 13 to 47 on 18 cores); runs swung by up to
2.5x, so read each group as a ratio within one run.

`cargo bench -p geode-diagnostics --bench log_filter` over a synthetic
4,096-record tail (four targets, five levels, ~70-character messages
`record {i}: partition 2026-09-27 · EU_TECH loaded {n} rows in {m} ms`):

| Group | Query | Median | What it is |
|---|---|---:|---|
| `log_table_4096` | `""` | 1.51 ms | `log_rows` + `log_table`, no narrowing |
| `log_table_4096` | `"ingest"` | 1.41 ms | a quarter of rows kept |
| `log_table_4096` | `"eutch ld"` | 4.95 ms | two words, every row kept, marks in the message |
| `log_table_4096` | `"zzq"` | 936 µs | every row rejected (formatting + subsequence reject) |
| `narrow_4096` | `"ingest"` | 597 µs | `Narrow::row` alone over pre-formatted text |
| `narrow_4096` | `"eutch ld"` | 3.26 ms | the same, worst case |

`cargo test -p geode-diagnostics --release -- --ignored log_rebuild_timing
--nocapture` (the page's headless `rebuild`, 20 runs): unfiltered median
3.95 ms (max 4.41 ms); `"eutch ld"` median 6.78 ms (max 7.17 ms).

The first `Narrow` drafts ran `eutch ld` at 19.6 ms (load ~47) and 7.0 ms
(load ~20) for the table build. A sample showed `palette::align` dominating:
it re-decoded both strings and allocated two score tables per call. Two
changes brought it to the figures above: `align_in` over pre-decoded
characters with caller-owned tables, and scoring only the columns between
the first occurrence of the query's first character and the last of its
last (no alignment lies outside them). ASCII characters lower without the
Unicode mapping.

The window also speeds the palette. `cargo bench -p geode-shell --bench
shell_cores -- palette/`, same load, before / after: `set_query_66_items`
16.2 / 13.4 µs, `set_query_500_items` 120.9 / 91.3 µs,
`set_query_2000_items` 464.6 / 344.8 µs; `fuzzy_match_one` 1.13 / 1.15 µs
(within noise: one short candidate gains nothing from the window).
