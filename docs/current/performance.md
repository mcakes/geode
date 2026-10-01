# Performance

Performance is an architectural contract. The UI thread renders prepared
state; data I/O and database work stay on background owners; large data remains
columnar; hot paths avoid per-frame allocation and repeated formatting.

## Budgets

| Path | Contract |
|---|---|
| Pure UI action | Under 8 ms |
| View requery at one million rows | Under 50 ms |
| Chart cache miss at the 500,000-point cap into 1,600 columns | Under 2 ms |
| Ingest and source work | Never blocks the UI or drops a foreground outcome |

These are path budgets, not general claims about every operation. Cold ingest,
document publication, and vendor latency have different shapes and must be
reported honestly rather than compared with a UI-frame budget.

## Instrumentation

`geode_shell::perf::FrameHistogram` records intervals between consecutive
`ShellView` renders during interaction. It uses fixed log buckets, saturating
counters, no allocation, and no notification. Intervals at or above the idle
cutoff are counted as idle gaps rather than frames. Diagnostics labels these
as frame intervals and shows each query stage’s sample count separately.
The interval histogram has logarithmic bucket labels and a separate count
above 100 ms. It does not classify render cadence against the 8 ms pure UI
work budget; those measurements have different meanings.

The `perf::toggle_overlay` action shows frame p50, p95, and maximum plus query,
snapshot-to-paint, and combined requery latency. It has no timer and therefore
does not wake an idle application merely to repaint statistics. `perf::reset`
clears the counters.

The optional `profiling` feature enables GPUI's profiler, frame overlay, input
latency histograms, and hang detection:

```sh
cargo run -p geode-app --features profiling
cargo check -p geode-app --features profiling
```

Release and benchmark profiles retain debug symbols for external profiling.

## Benchmarks

```sh
cargo bench -p geode-shell
cargo bench -p geode-data
cargo bench -p geode-blotter
cargo bench -p geode-documents
cargo bench -p geode-marketdata
cargo bench -p geode-chart
cargo bench -p geode-timeseries
cargo bench -p geode-pricer
cargo bench --workspace --no-run
```

Criterion stores local comparisons under `target/criterion`. CI compiles every
benchmark but does not reject a change from noisy wall-clock thresholds on
shared runners.

## Current reference measurements

Reference values below are medians recorded on Apple silicon with release or
benchmark profiles. They are orientation points, not portable promises; use
the measurement log for fixture and hardware details.

| Path | Shape | Median |
|---|---|---:|
| Warm database reopen | populated demo store | 5.0 ms |
| View requery | 1,000,000 rows, no text filter, depth two | 2.51 ms |
| Series query | four slots plus ratio, daily over one year | 9.56 ms |
| Series query with stats | two minute slots over one month | 14.5 ms |
| Chart path rebuild | 500,000 points into 1,600 columns | 1.51 ms |
| Timeseries chart model | 500,000 buckets × four slots | 259 µs |
| Blotter fully expanded flatten | 720,881 result nodes | 1.18 ms |
| Blotter selection summary | 720,881 rows, every measure column | 1.20 ms |
| Market-data pivot build | 20 × 30 CVI grid | 285 µs |
| Market-data flat build | 10,000 × five values | 8.18 ms |
| Market-data cell patch | 10,000 × five values | 116 ns |
| Line-pricer sheet shift + undo | 1,000 entries / 1,200 sheet rows | 1.52 ms |
| Line-pricer single cell edit + undo | 1,000 entries / 1,200 sheet rows | 6.66 µs |
| Line-pricer grid build | 1,000 entries / 1,200 sheet rows, every package open, each package row's `/`-joined leg values | 1.42 ms |
| Line-pricer scope apply | 1,000 entries / 1,200 sheet rows, three-term expression plus text filter, half hidden | 1.92 ms |
| Line-pricer scoped grid build | the same sheet under that scope | 692 µs |
| Line-pricer flat rebuild | 1,000 entries / 1,200 sheet rows, every package open: the empty-chain rollup plus the grid build, as the tile runs it | 1.68 ms |
| Line-pricer grouped rebuild | 1,000 entries over four underlyings × three expiries under `[underlying_ref, expiry, position_ref]`, every group and package open: rollup plus grid | 3.15 ms (rollup 335 µs) |
| In-process scope evaluation | one row, three-term expression plus text filter | 570 ns |
| Scope expression suggestion refresh | 20,000 cached values, ranked and capped at 50 | 6.82 ms |

The flat 10,000-row market-data build sits at the UI budget boundary. Ordinary
cell commits use the constant-time patch path; deliveries and structural row
changes still rebuild.

## Cache and allocation contracts

- A blotter formats the visible window into the shared
  `geode_tile::grid::WindowCache` rather than formatting in `render_td`.
- A market-data delivery or structural edit builds a `MatrixModel`; an
  ordinary cell commit patches it.
- `ChartKey` contains everything timeseries chart preparation reads. Cursor
  movement and fetch-state changes reuse value vectors. Per-slot visibility
  changes rebuild the model; theme and named-color changes can trigger that
  rebuild during render.
- Chart data paths and chrome are cached. A view move invalidates geometry but
  does not rebuild the module's data model.
- Chart decimation reuses buffers and retains up to two extrema per finite
  run in each pixel column. Gaps can increase output beyond two points per
  column. Warm paints still allocate for path submission, labels, and tooltips.
- Density bars are uncached but capped at 2,000 quads per chart paint.
- A timeseries view move keeps at most one statistics request in flight and
  asks for the latest window when it answers, so statistics refresh at the
  query's own rate during a pan rather than being interrupted by each event.
- A pricer grid model is rebuilt on edit, delivery, expansion, view, clock or
  entry change, never in render; paints are a per-theme memo. Every rebuild
  first re-evaluates the frame's scope over every line (`apply_scope`),
  synchronously on the UI thread; the two together are the 8 ms budget.
- Config dialogs derive rows at each render, key-handling, and click-resolution
  call site; they do not retain a row cache. Small row sets have measured costs
  in the tens of microseconds. Keybinding resolution repeatedly scans bindings
  for actions and candidates, so its cost grows with the action registry and
  user overrides. See the [measurement log](../perf.md).
- Large module tables use virtualization or prepared visible rows.

## Known gaps

- A real painted frame is not covered by headless Criterion benchmarks. Exact
  GPU submission, text, popup, and whole-window costs need display profiling.
- Series statistics repeat the bucketing prefix for points, percentiles, and
  density. It remains within budget for measured ranges; a shared materialized
  result or grouping-set query is the next step if wider statistics become
  expensive.
- A timeseries statistics request also recomputes the points over the whole
  range, which a view move does not change. A statistics-only request would
  shorten each refresh during a pan.
- Series append deduplication reads every stored version for the pair. Large
  historical pairs may need a narrower live-value index.
- Measure and feed-document live/archive retention has no production
  scheduler; the sweep API is exercised by tests. Their archives can grow
  without that automatic bound. Local documents (pricer sheets) are swept to
  200 archived generations; series retention runs during append.
- While the diagnostics page is visible, every publication (each sheet autosave
  included) rebuilds the catalog on the service thread, listing every
  generation of every sheet (up to 201 each), and the diagnostics entity
  compares the new snapshot whole on the UI thread. Unmeasured; with hundreds of sheets it may need
  a narrower catalog read.
- Diagnostics perf rows sample requery and catalog resource metrics on their
  next rebuild; those inputs have no dedicated perf invalidation. Histogram
  copying compares sample count and maximum, so idle-only changes and a
  reset/refill with the same count and maximum can be missed.
- The measured parallel CSV result covers `read_csv`, not the complete staging
  pipeline. Concurrent staging is on hold until the real path and a network
  share are measured.
- CI compiles benchmarks but has no stable regression baseline.

## Recording a measurement

Put the durable contract and current interpretation in this guide. Append raw
results, fixture construction, hardware, toolchain, discarded approaches, and
rerun variance to [`docs/perf.md`](../perf.md), which is the chronological
measurement log. State what the number includes and excludes. Do not turn a
microbenchmark into an end-to-end claim.

## Blotter search opening

`cargo test -p geode-blotter fzf_open_large_snapshot -- --nocapture` exercises
opening `/` over 100,000 descendants, first paint, full indexing, and reopening
with the cached index. Timings are diagnostic, with no wall-clock assertion.
The test verifies that parents and the last descendant are navigable before
indexing, that the last descendant remains searchable afterward, and that
reopening reuses the prepared dataset. Set `GEODE_FZF_TEST_ROWS=1500000` to
exercise 1.5 million descendants.

Display now uses all loaded rows immediately instead of a 64-row preview.
The snapshot supplies a shared depth-first row order prepared on the query
worker (four bytes per reachable row). A local sort reuses the table's order
when fully expanded, otherwise computes the sorted full order on opening.
The initial search order is an implicit range, avoiding a second identity
permutation allocation for all rows on the UI thread. Only visible labels and
cells are formatted; full search indexing and search
topology preparation run in the background. Visible chevrons use the existing
snapshot tree until the search topology is ready. Missing grouping levels still require a database query.

On the development build measured on 2026-09-29, opening over 1.5 million
descendants with the ranked-tree presentation took 35 ms and first paint 68 ms,
while indexing completed at 3.97 seconds. All loaded parents and leaves were
already available before the worker completed. These are headless fixture
timings, not release-app latency;
see [the performance log](../perf.md) for details and earlier measurements.

## Search typing

Fzf ranking stores row indices, calculates highlights only for visible rows,
and narrows from the last completed candidate set when a single-word query is
extended. Backspacing, other edits, multi-word queries, and dataset replacement
use the full index. A reusable score-only matcher handles ASCII words with the
same ordering as the original matcher; Unicode and multi-word queries retain
the original scorer after a cheap rejection filter. All results remain available
for navigation. Tree results retain each ancestor once, with sibling branches
ordered by their strongest match. The worker links nodes in match-discovery
order and traverses those links, avoiding a second sort of all results. Folding
also runs on the worker and retains the direct-match candidates for narrowing.

An optimized ranking-only measurement over 1.5 million synthetic rows on
2026-09-29 reduced broad prefix queries from 395–1,261 ms to 15–20 ms. Refining
`149` to `1499` took 8–9 ms, versus 513 ms for the original full scan. These
measurements exclude index construction, input dispatch, and rendering; broad
multi-word searches that survive the rejection filter can still cost more.
With hierarchy ordering included, a subsequent run measured 27 ms for broad
prefixes and 8.9 ms for `1499`; these likewise exclude input and rendering.
Fixture and measurement details are in [the performance log](../perf.md).
The ignored `fzf_rank_large_index` test provides the same query sequence for
future diagnostic runs with `cargo test --release -p geode-shell
fzf_rank_large_index -- --ignored --nocapture`.
