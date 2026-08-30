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
