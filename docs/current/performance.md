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
| Store handoff, background collector to app (`acquire_app` to an open store) | Under 500 ms typical; the cap is the 2 s release drain plus the checkpoint |

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

### Memory

`geode_shell::memory::sample` reads this process's memory from the operating
system on every 500 ms reload-poll tick, whether or not diagnostics is open:

- macOS: `task_vm_info.phys_footprint` and its lifetime peak
  (`ledger_phys_footprint_peak`). This is Activity Monitor's Memory column.
  Unlike resident size it counts dirty pages that were compressed or
  swapped out, so it does not fall merely because the system is under
  memory pressure.
- Windows: `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` (private bytes) and
  `PeakPagefileUsage` (peak private commit). Private bytes count committed
  memory whether resident or paged out; the working set is not used.
- Other platforms read nothing; the page shows the process row as not
  available.
- A failed read is skipped silently and the tick does nothing else with
  memory. Before any success the page shows the row as not available. After
  one, the page keeps showing the last copied reading with no stale marker,
  the tracker is not advanced, and the debug cadence pauses: the next
  successful sample is compared with the last successful one, and logs its
  debug line if a minute has passed since the last.

A pure `MemoryTracker` keeps current, a peak that never falls, and the wall
time of the tick that first saw the peak (the first sample's peak may predate
it, since the operating system's peak covers the whole process). It logs on
the `geode::memory` target (`[log] memory = "debug"`):

- `info` once at the first sample as the baseline;
- `info` when the peak rises at least 256 MiB, or at least 25%, above the
  last *logged* peak, so slow growth logs once it accumulates;
- `debug` with current and peak at most once a minute, the first a minute
  after the baseline.

The poll copies the reading into `Diagnostics` only while the page is
watched, and then only when one of these holds:

- it is the first refresh since the page became watched (so a reopened page
  never shows the reading and peak time it had when hidden);
- current has moved from the copied value by at least
  `memory::current_hysteresis`: the larger of 128 MiB and 5% of the copied
  value;
- the displayed peak text (`memory::format_bytes`, one decimal of the largest
  binary unit) changes.

An idle macOS footprint was measured moving about 90 MB between ticks, which
crosses one-decimal display steps on irregular ticks; comparing current at
display granularity would repaint an idle page. Under the hysteresis such
jitter copies nothing. The peak never falls, so its text changes only on
growth and cannot alternate. The guarantee is therefore: a footprint that
moves less than the hysteresis around its last copied value, under a steady
peak, never notifies, preserving the `IDLE_CUTOFF` guarantee that a
poll-spaced redraw cannot sustain a repaint loop. Real growth or release past
the hysteresis notifies once per crossing. The cost is accuracy: the page can
show a current up to the hysteresis away from the live value, and a peak rise
too small to change the displayed peak leaves the previous peak time shown.

DuckDB's figures come from the catalog snapshot instead: `duckdb_memory()`
in use and its three largest non-zero tags, `duckdb_temporary_files()` spill
bytes, and the `memory_limit` setting. DuckDB reports the limit only as text
with one truncated decimal (`"38.3 GiB"`), so the parsed figure can be up to
a tenth of a unit low; unreadable text warns on `geode::query` and shows as
an unknown limit. Geode does not set `memory_limit`; DuckDB's default is 80%
of physical memory. A limit of 1 PiB or more, which is how DuckDB reports
`memory_limit = '-1'` (16383.9 PiB), shows as no limit.

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
cargo bench -p geode-volslice
cargo bench -p geode-pricer
cargo bench -p geode-diagnostics
cargo bench -p geode-classifications
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
| Store handoff, collector idle | `--demo 100000`: `acquire_app` to `Store::open` | 222 ms |
| Store handoff, collector just opened | the same, its bus's startup burst in flight (about 130 documents drained) | 828 ms |
| Store handoff, a large file loading | 1,000,000-row CSV under way (512MB `memory_limit`); 2,000,000 rows (1GB) | 1.93 s; 4.46 s |
| View requery | 1,000,000 rows, no text filter, depth two | 2.51 ms |
| View requery, no context columns | 1,000,000 rows, underlying-grain measures only, depth two | 20.5 ms |
| View requery, roster's context columns | the same view with `underlying_ref`, `position_ref`, `instrument_ref` | 35.0 ms |
| View requery grouped by a classification (`query_classification`) | 1,000,000 rows, `sector > underlying_ref`, two underlying-grain measures; `sector` maps the 10 observed underlyings / 5,000 values (padded); full depth / depth two; loaded machine (load 33 to 58) | 24.0 / 14.3 ms; 55.3 / 51.2 ms |
| Series query | four slots plus ratio, daily over one year | 9.56 ms |
| Series query with stats | two minute slots over one month | 14.5 ms |
| Chart path rebuild | 500,000 points into 1,600 columns | 1.51 ms |
| Timeseries chart model | 500,000 buckets × four slots | 259 µs |
| Blotter fully expanded flatten | 720,881 result nodes | 1.18 ms |
| Blotter selection summary | 720,881 rows, every measure column | 1.20 ms |
| Market-data pivot build | 20 × 30 CVI grid: the `MatrixIndex` | 120 µs |
| Market-data flat build | 10,000 × five values: the `MatrixIndex` (labels and row facts, no cell text) | 586 µs |
| Market-data window fill | 40 × five values | 17.7 µs |
| Market-data cell patch | 10,000 × five values: one-cell window refill | 105 ns |
| Market-data `/` open cells | 10,000 × five values: the find table's first window (64 rows) then its 40 shown rows, into a `RowCache` | 53 µs |
| Line-pricer sheet shift + undo | 1,000 entries / 1,200 sheet rows | 1.52 ms |
| Line-pricer single cell edit + undo | 1,000 entries / 1,200 sheet rows | 6.66 µs |
| Line-pricer grid build | 1,000 entries / 1,200 sheet rows, every package open: the `GridIndex`, no measure text | 271 µs |
| Line-pricer window fill | 40 rows | 29.2 µs |
| Line-pricer scope apply | 1,000 entries / 1,200 sheet rows, three-term expression plus text filter, half hidden | 1.92 ms |
| Line-pricer scoped grid build | the same sheet under that scope | 114 µs |
| Line-pricer flat rebuild | 1,000 entries / 1,200 sheet rows, every package open: the empty-chain rollup plus the index, as the tile runs it, before its window fill | 343 µs |
| Line-pricer grouped rebuild | 1,000 entries over four underlyings × three expiries under `[underlying_ref, expiry, position_ref]`, every group and package open: rollup plus index, before its window fill (a cold grouped 40-row fill adds 206 µs) | 610 µs |
| Line-pricer refill-only delivery | 1,000 entries / 1,200 sheet rows, structure unchanged | 241 µs |
| Line-pricer reference fill | 1,000 entries / 1,200 sheet rows, every line blank: one `fill_currencies` batch, packages folded once (folding per filled line measured 13.7 ms) | 17.4 µs |
| Line-pricer sorted rebuild | the flat rebuild above under an `npv` descending sort over varied prices: rollup, `sort::rank`, index (heavily loaded machine; the unsorted rebuild measured 469 µs in the same run) | 549 µs |
| Line-pricer sorted grouped rebuild | the grouped rebuild above under the same sort (the unsorted one measured 834 µs in the same run; the rank alone 102 µs) | 923 µs |
| Classifications rebuild after an edit | 5,000 source values, half labelled, `rows` desc sort and a `/` filter active: `History::apply`, `to_toml`, `classification::rows` and the grid's relabel rebuild, before table preparation and paint (load 4.5) | 1.53 ms |
| Classifications rows rebuild | the same grid, rows and grid only (a values answer or a reload) | 1.21 ms |
| In-process scope evaluation | one row, three-term expression plus text filter | 570 ns |
| Scope expression suggestion refresh | 20,000 cached values, ranked and capped at 50 | 6.82 ms |
| Keybinding rows | builtin + 400 synthetic module actions, 200 user overrides: derive and rank, per input change | 667 µs |
| Keybinding filter keystroke | the same, prepared re-rank | 113 µs |
| Object browse rows | 500 views: derive and rank, per input change | 188 µs |
| Object browse filter keystroke | the same, prepared re-rank | 240 µs |
| Palette paint | 551 items, the rows in view, prepared labels | 275 ns |
| Diagnostics Log keystroke | full 4,096-record tail, a query every record matches (two to five words, or one 19-character word): the page's `rebuild` on the UI thread, timed inside the update, which shows the held answer and starts the narrowing (load average 22 to 39 on 18 cores) | 0.41 to 0.54 ms |
| Diagnostics Log settled rebuild | the same tail under a held narrowing (records or gates changed, query unchanged; unfiltered 0.47 ms) | 0.60 to 0.95 ms |
| Diagnostics Log cache fill | `LogCache::sync` formatting and lowering all 4,096 records cold, on the UI thread: paid when the Log is first shown over a full tail and after a clock change, then only for new records (load 29 to 31) | 5.14 ms |
| Diagnostics Log narrowing, off the UI thread | the same: `log_cache::Narrowed::run`, two words / three / five / one 19-character word | 6.1 / 10.2 / 12.1 / 8.5 ms |

Production view queries also carry the roster's context columns
(`underlying_ref`, `position_ref`, `instrument_ref`; see
[context columns](data-path.md)), which the view requery row above does not
set; the `query_context` bench group measures a view with and without them. Those
two rows were recorded on a heavily loaded machine, so read them as a ratio
rather than as reference figures: on this shape the three context columns cost
about 70% more, mostly the position- and instrument-grain scans no shown
measure already reads. Re-measure on an idle machine before quoting them.

A derived dimension is a map probe per row, not a `CASE` with one arm per
value: at 5,000 values the `CASE` took 2.7 s against 26 ms at ten, and the
probe 53 to 65 ms against 12 to 24 ms, all on a loaded machine. A
5,000-value classification therefore still sits at or just over the 50 ms
budget there, and the probe's cost still grows with the classification's
size. An idle re-measure is owed; the next lever is a keyed lookup table
joined on the unique source value, if the idle figure stays over budget.

## Cache and allocation contracts

- A blotter formats the visible window into the shared
  `geode_tile::grid::WindowCache` rather than formatting in `render_td`.
- A fuzzy `/` result table in market data and the pricer formats only the
  rows it shows. The shell's find table reports them (`set_table`'s
  `on_rows`) on each range the table reports and at the next layout after
  every result-set change; the module formats them into a
  `geode_tile::grid::RowCache` keyed by document row, keeping rows still
  shown and dropping the rest, so it holds about a screenful. Before the
  table's first report it reports the first 64 rows. The paint callback only
  reads; a miss paints blank.
- The blotter's `/` table formats only the rows it reports
  (`delegate::FindCells`, a `RowCache` by source row) with the grid's own
  formatter. A new snapshot re-indexes and clears every held cell before the
  re-report; the search index arriving for the display `/` opened on keeps
  them. Its footer strings are prepared when `shown`, the semi-join or the
  placement changes, and each dataset time is parsed once when the header
  model is prepared; render and the stale timer compare the parsed times.
- A market-data delivery, structural edit or bulk step builds a `MatrixIndex`
  (labels and row facts, no cell text) and refills only the window the table
  last reported; a one-cell commit refills one window cell. The session tick
  builds nothing.
- A market-data panel emitting into a link group assembles its draft
  document once per changed draft, not once per pull: the rows are cached on
  the document key, the painted base snapshot and the draft, and a pull with
  none of them moved returns the same allocation. An edit costs nothing
  until the shell pulls. The assembly itself walks the whole document and is
  unmeasured.
- An emitting blotter or pricer recomputes one row per pull: the cursor
  row's underlying, with no selection walk. The shell pulls on every
  notification of an emitting tile; an emission equal to the tile's last
  writes nothing and notifies nobody.
- A draft edit on an emitting panel bumps the board watches of the keys it
  changed, never the frame's `data` version, so it requeries no tile that
  watches published data, and it is not staged behind a flip barrier.
- A link group's scope change advances the frame generation the session
  writer polls, and the writer then serializes a snapshot on its 500 ms
  tick; a snapshot whose text, without the groups' scopes, equals the last
  one extracted is not written and is not serialized a second time with
  them, so an emitting tile's cursor does not rewrite `session.toml`.
- `ChartKey` contains everything timeseries chart preparation reads. Cursor
  movement and fetch-state changes reuse value vectors. Per-slot visibility
  changes rebuild the model; theme and named-color changes can trigger that
  rebuild during render.
- Chart data paths and chrome are cached. A view move invalidates geometry but
  does not rebuild the module's data model.
- A vol slice tile builds its xy model when a vol batch answers
  (`core::build::model`), never in render. Its header text, strip rows and
  footer notice are prepared when an input they read changes; a cursor step
  rebuilds no chart path and formats no strip text. A split step is the same
  slots under a new model version, and a view move keeps the model.
- Chart decimation reuses buffers and retains up to two extrema per finite
  run in each pixel column. Gaps can increase output beyond two points per
  column. Warm paints still allocate for path submission, labels, and tooltips.
- Density bars are uncached but capped at 2,000 quads per chart paint.
- A timeseries view move keeps at most one statistics request in flight and
  asks for the latest window when it answers, so statistics refresh at the
  query's own rate during a pan rather than being interrupted by each event.
- A pricer grid index is rebuilt on edit, structural delivery, expansion,
  view, clock or entry change, never in render; measure cells are formatted
  only for the window (`CellPass`), and paints are a per-theme memo. Every
  rebuild first re-evaluates the frame's scope over every line
  (`apply_scope`), synchronously on the UI thread; the two together are the
  8 ms budget.
- A price delivery whose effective chain and rollup are exactly unchanged
  refills only the window: no index rebuild. Any difference, a landed line,
  or a NaN group value rebuilds. The scope and the rollup are still
  re-derived on every delivery, because a price can move a line in or out of
  the scope or between groups.
- The keybindings dialog and the object dialog's Browse stage hold prepared
  rows (`geode_shell::prepared::Prepared`):
  derived from exactly the inputs they read, ranked for the query, and read by
  render and by every handler, so a key or click acts on the painted rows. A
  query change re-ranks without re-deriving. The shell refreshes them at its
  event seams (`ShellView::refresh_dialog_rows`: open, each dialog key, the
  dialog input's Change, each dialog pointer transition, an applied reload,
  and the reveal of a covered object dialog); render never refreshes, and a
  debug-build assertion in the dialog's `build` refuses a stale list. Both
  lists are keyed by the config revision that every applied reload bumps; the
  action registry is fixed once the shell is built, and an object dialog's
  domain is fixed for its life. An in-dialog rebind, unbind or reset, and an
  object create, delete, revert or fork, change the configuration only through
  `apply_reload`. A refresh after an input change derives and ranks: about
  0.67 to 0.78 ms for 507 keybinding actions with 200 user overrides, 0.19 to
  0.40 ms for 500 browse objects. A filter keystroke re-ranks without
  deriving: 113 µs and 235 to 245 µs. A repaint derives and ranks nothing.
  Settings and the object dialog's Edit and Column stages are not
  prepared: they derive their rows at each render, key-handling and
  click-resolution call site, because one derivation and rank costs 2 to
  3.5 µs for settings and 2 to 13 µs for the largest demo edit draft, too
  little to repay a cache key and its refresh seams. See the
  [measurement log](../perf.md).
- The palette prepares each item's title and category once per open and its
  highlight ranges once per query, beside the cached ranking; the list is a
  `uniform_list`, so a paint touches only the rows in view and formats no
  titles or highlights; each row's `kbd` binding chips still format per
  paint. Selection scrolling keeps the selected row in view
  (`ScrollStrategy::Nearest`). Ranking and frecency are unchanged: usage
  bonuses are fixed for the open palette, and a dispatch closes it first.
- Status-bar text is prepared where its input changes (the matcher's count,
  the reload outcome, write and restart messages, the cached diagnostics
  summary, the cached scope-bar model's `AS OF` label, the theme service's
  active name); the bar's signature takes `&SharedString`, so a paint clones
  reference counts and formats no segment text. The pending-key chips still
  format through `kbd` while a sequence is in flight.
- Large module tables use virtualization or prepared visible rows.

## Known gaps

- The Diagnostics Log's fuzzy narrowing over a full tail exceeds the 8 ms UI
  budget for phrase queries (6 to 12 ms on the reference machine, loaded, and
  more on a slower one), so it runs on the background executor and the UI
  thread only builds the table from the held answer (under 1 ms). The cost:
  after a keystroke the table shows the previous answer for one pass, and
  records arriving under a query appear after their own pass. A changed query
  always re-narrows the whole tail; extending the previous query is not used
  to narrow only its kept rows, because the greedy word placement does not
  guarantee that a row the longer query keeps was kept by the shorter one.

- A classification CSV import is planned on the background executor (60 to
  93 ms on the UI thread at the 10 MiB / 100,000-row limits, before it
  moved), but applying the plan at `y` (`ImportPlan::apply`, `to_toml` and
  the grid's relabel rebuild) still runs on the UI thread: about 28 ms at
  100,000 changed rows, over the 8 ms budget. Imports of a few thousand rows
  stay within it. Moving the apply off the thread would need the history
  and the pending object to accept a result computed over a copy.

- Object-dialog paint still formats per-row element ids (layer, override,
  drift badges; field and provenance ids) and resolves the Colors browse
  swatches per paint; the rows themselves are prepared. Bounded by the
  domain's object count.
- The keybindings list is not virtualized: each repaint builds elements for
  every row and formats each row's binding chips (`kbd::binding`), though it
  derives and ranks nothing. Bounded by the action registry (about 500
  actions).
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
- The process memory peak time shows hours, minutes and seconds only; a peak
  reached on an earlier day reads as that time of day. The DuckDB memory rows
  are as fresh as the last catalog snapshot, which is read on publication
  while the page is visible, not on the memory poll.
- Diagnostics perf rows sample requery and catalog resource metrics on their
  next rebuild; those inputs have no dedicated perf invalidation. Histogram
  copying compares sample count and maximum, so idle-only changes and a
  reset/refill with the same count and maximum can be missed.
- The measured parallel CSV result covers `read_csv`, not the complete staging
  pipeline. Concurrent staging is on hold until the real path and a network
  share are measured.
- CI compiles benchmarks but has no stable regression baseline.
- The store handoff misses its 500 ms target in two shapes. Right after the
  collector opened, its release drains the demo bus's startup burst (about
  130 documents) and the handoff takes about 0.8 s. With a file load under
  way the release lets the load finish, so the handoff is the rest of that
  load: about 1.9 s at 1,000,000 rows and 4.5 s at 2,000,000, past the 2 s
  drain cap; a load still running after 12 s ends the collector at its
  release watchdog, and the app opens then (the load is rolled back and
  rediscovered). Idle, it is
  about 0.1 to 0.25 s, quantized by the collector's 100 ms hold poll and the
  app's 100 ms open retry.
- A finite `[collector] memory_limit` of 512MB made DuckDB abort the
  collector on large CSV loads (an internal assertion in its temporary
  memory manager): on the second 2,000,000-row file in one process, and
  intermittently after about 3,000,000 rows of 1,000,000-row files. 1GB
  and 8GB loaded the same files. The limit is therefore unset by default
  (DuckDB's own, as the app); the overnight footprint measurement is owed
  before a value is chosen.
- The vol slice `model_build` bench (`cargo bench -p geode-volslice`) measured
  about 200 µs with 1,000-point curves under heavy machine load (see the
  measurement log). It times `core::build::model` alone, the work a repaint
  does on the UI thread when a batch answers, over twelve active monthly
  expiries with the published CVI, a draft and a 60-strike chain at each,
  densities on and two differences (`cvi draft − chain` and `cvi − cvi
  draft`), in moneyness, the batch answered once by the stand-in model
  outside the timed loop. Each active expiry's color and companion are
  resolved there once through `HuePalette` (an OKLCH conversion and a
  contrast bisection each). Its target is under 1 ms. The delta coordinate
  (reversed, with NaN density gaps) is not benchmarked.
- The pricer's `/` cells read the live sheet through the index `/` built. A
  price-only delivery (the refill-only path) drops them and has the table
  re-report, so the rows shown refill from the new prices before the next
  paint. Once the tile installs another index (a regroup, a structural
  delivery, an expansion), the next row report drops every cell and the
  measure columns paint blank until `/` is reopened, rather than read rollup
  nodes and sheet rows through a stale index; the status line reads "Results
  out of date — reopen /" so a blank does not read as an unpriced line.
- The pinned table never reports a visible range of one row; a tile scrolled
  to a single row outside its prepared window can therefore paint it blank
  until another invalidation. Grid tiles cover initial one-row results by
  seeding the first 64 rows, and `WindowRequest::refill_range` refills the
  tail when a shrink moves the old window past the end. Fuzzy `/` tables
  also report their rows at layout after a result change, so narrowing to
  one match does not depend on the table's omitted callback.

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
