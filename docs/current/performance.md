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
cutoff are counted as idle gaps rather than frames.

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
| Market-data pivot build | 20 × 30 CVI grid | 285 µs |
| Market-data flat build | 10,000 × five values | 8.18 ms |
| Market-data cell patch | 10,000 × five values | 116 ns |
| Line-pricer sheet shift + undo | 1,000 rows | 1.52 ms |
| Line-pricer single cell edit + undo | 1,000 rows | 6.66 µs |

The flat 10,000-row market-data build sits at the UI budget boundary. Ordinary
cell commits use the constant-time patch path; deliveries and structural row
changes still rebuild.

## Cache and allocation contracts

- A blotter formats the visible window into a cache rather than formatting in
  `render_td`.
- A market-data delivery or structural edit builds a `MatrixModel`; an
  ordinary cell commit patches it.
- `ChartKey` contains everything timeseries chart preparation reads. Cursor,
  fetch-state, and visibility changes do not copy the value vectors.
- Chart data paths and chrome are cached. A view move invalidates geometry but
  does not rebuild the module's data model.
- Chart decimation reuses buffers. GPUI path submission still clones the
  decimated path, and component axis painters allocate small tick vectors.
- Density bars are uncached but capped at 2,000 quads per frame.
- Config dialogs derive small row sets on change. Large module tables use
  virtualization or prepared visible rows.

## Known gaps

- A real painted frame is not covered by headless Criterion benchmarks. Exact
  GPU submission, text, popup, and whole-window costs need display profiling.
- Series statistics repeat the bucketing prefix for points, percentiles, and
  density. It remains within budget for measured ranges; a shared materialized
  result or grouping-set query is the next step if wider statistics become
  expensive.
- Series append deduplication reads every stored version for the pair. Large
  historical pairs may need a narrower live-value index.
- Document generations are not yet connected to the directory-source
  retention sweep, so a long-running subscribed document source can grow its
  archive without that bound.
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
