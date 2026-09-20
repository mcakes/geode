# Geode — Timeseries Viewer

**Date:** 2026-09-19
**Status:** Approved in brainstorm; implementation plan to follow
**Governs:** the timeseries viewer module and the three platform pieces
it needs: the `series` dataset family, the on-demand `Fetch` adapter
shape, and the `geode-chart` crate. It is roadmap sub-project F's
second half, taken ahead of slice 2 (see §3).
**Conforms to:** `docs/PHILOSOPHY.md`, the foundation design
(`2026-08-28-geode-foundation-design.md`), the modules roadmap
(`2026-09-12-geode-modules-roadmap.md`), the market-data documents
design (`2026-09-12-geode-market-data-documents-design.md`, for the
adapter tier it extends and the panel conventions it reuses), and the
2026-08-29 chart rendering spike
(`docs/superpowers/spikes/2026-08-29-gpui-chart-rendering-spike.md`).

## 1. Scope

### 1.1 What this delivers

A tile that plots one or more timeseries fetched on demand from a
named source, over a date range, at a display frequency, with:

- a per-series manager that costs no space when idle (header chips)
  and opens as a popup: show/hide, y-axis, colour, bucket rule, remove;
- composition of loaded series by arithmetic expression, each result
  a series of its own;
- optional sideways density histograms and dashed percentile lines,
  both over the visible window;
- a session (trading-time) x-axis by default, wall clock on request;
- keyboard zoom and pan, mouse crosshair and readout;
- a seeded demo adapter under `--demo`, with two demo sources.

Built in four parts (§12): the data tier, the series query, the chart
crate, the module.

### 1.2 Done state

1. `cargo run -p geode-app -- --demo`, add a timeseries tile, `:add
   SPX.close`, and a line paints for the default `1y` at `1d`; `:add
   VIX`, `y` on its chip moves it to the right axis and `y` twice
   more opens a lower pane for it; `x` and
   `SPX.close / VIX` paints a third line; `D` and `p` paint the
   density strip and `p5 p50 p95`; `r` widens the range and only the
   missing span is fetched; `f` steps to `1h` and the query, not a
   fetch, answers. **As built (Part 4): built; display check
   pending** — the sandbox has no window, and §9.13 lists what to
   look at.
2. A series query over one million cached rows returns under 50 ms;
   decimating 500,000 points to pixel columns and rebuilding the paths
   takes under 2 ms; a frame with nothing changed allocates nothing
   in the chart element. **As built (Part 3): the last clause is too
   strong and §8.5 restates it.** An unchanged frame REBUILDS nothing
   — no decimation, no tessellation, no `xs` refill, no chrome
   derivation — but the pinned `Window::paint_path` takes its path by
   value, so a cache HIT still clones and translates it: one vertex
   `Vec` per painted path, bounded by the decimated point count, plus
   the chrome `Vec`s the component's own painters take. Both are the
   pinned API's price and neither is O(the data).
3. A corrected value for an existing `ts` wins live and loses under an
   as-of set before its `received_at`; an overlapping refetch does not
   grow the table; a tile restored from `session.toml` refetches once
   and paints what it painted before. **As built (Part 4): built;
   display check pending** — the first two clauses are pinned in
   `geode-data`, the third by the tile's own
   `a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once`.
4. Every behaviour in §11.2 has a harness entry naming its test, and
   `--anchors-only` is clean.

### 1.3 Explicitly not built

Each item names the seam it will use, so none is a redesign:

- **Live tail.** The append path (§4.4) takes rows from any producer;
  no producer pushes into it yet. A tailing adapter is a
  `Subscription` whose parser yields `SeriesRows` for a pair and hands
  them to the same append function. The demo adapter grows a tail in
  a later part.
- **Real adapters.** KDB and REST shims implement `Fetch` (§5.2)
  blind, behind cargo features, per roadmap ruling 3 and 5.
- **Expression functions.** The grammar (§7) is arithmetic only.
  `lag`, `mean`, `pct_change` and friends are window functions in the
  compiler and one enum arm each in the AST; the wall is named in §2
  ruling 8.
- **Chart layouts as config objects.** A tile's series list lives in
  the session only. A named layout is an object-dialog domain later.
- **More than two panes.** `Axis` has four values over two panes
  (ruling 12); a third pane is a `Vec` where there is a pair.
- **Launch with context** (the blotter opening a timeseries tile for
  the underlying under its cursor) waits for the roadmap's slice 2
  mechanism; the factory already accepts restored state, which is the
  door it will use.
- **Smoothed density curves.** Ruling 6 chose histogram bars; a kernel
  over the same bins is a pure function in `geode-chart` if bars prove
  hard to read.
- **Holiday calendars.** The session axis (§8.2) makes them
  unnecessary for display.

## 2. Rulings

Made by the user in the 2026-09-19 brainstorm and binding on every
part:

1. **Composition, percentiles and density are view-shaping and run as
   SQL in the data tier.** A diff or ratio is a join and a pointwise
   operation, a percentile is a rank aggregate, a histogram is a
   grouped count; none needs financial reasoning to be correct. The
   module never does arithmetic on values. This also settles the
   roadmap §7 question deferred to slice 3.
2. **Frequency is applied on display, by DuckDB, never by the source.**
   The cache holds each series at the source's native grain because
   not every source can resample. The bucket rule is `last` by default
   and settable per series to `first`, `mean`, `min`, `max`.
3. **A series is identified by `(identity, source)`.** The identity is
   an opaque string the source interprets; a source may offer a
   catalogue of identities for typeahead and need not. Written
   `SPX.close@kdb_hist`; identity first, because that is the order a
   trader thinks in. A user preference `[timeseries] default_source`
   makes the suffix optional.
4. **One date range per tile.** Every series in a tile shares the
   x-axis and the range; widening fetches only what is missing. The
   frame's as-of clips the visible end. Default `1y`.
5. **The series manager is header chips plus a popup** (placement A of
   three mocked up): the legend is chips in the module's own header
   strip; `L` opens a popup over the plot with the full rows and the
   verbs.
6. **Density is a sideways histogram**, bars straight from DuckDB bins,
   drawn in a strip at the plot's right edge, one per visible series
   in its colour, aligned to that series' axis. Curves later if bars
   prove hard to read.
7. **Composition is an expression field**, not a two-operand verb.
8. **The expression language is arithmetic only**: series references,
   numeric constants, `+ - * /`, parentheses. A rolling or lagged form
   is one step from a realised-vol formula, and that is the charter
   wall.
9. **The series family is its own family, bitemporal and append-only**,
   not a reuse of the document family: documents replace whole, series
   extend, and live tailing is expected. Reasoning in §4.
10. **Percentiles default to `5 50 95`** and both percentiles and
    density are computed over the visible window, not the loaded
    range.
11. **Where one operand of an expression has no bucket, the result has
    none.** A carried-forward value would be an invented one.
12. **Four y-axes over two panes** (spec review, 2026-09-19): `left`,
    `right`, `bottomleft`, `bottomright`. While any visible slot uses a
    bottom axis the plot area splits horizontally into an upper and a
    lower pane sharing the x-axis; otherwise there is one pane. The
    fraction the upper pane takes is a tile setting.

## 3. Amendments to earlier designs

- **Roadmap §6 order.** Sub-project F's timeseries viewer is built
  before slice 2 (scenario panel). The fetch adapter shape is therefore
  shaped by this viewer rather than by the vol viewers; the vol viewers
  inherit `Fetch`, the series family and `geode-chart` unchanged.
- **Roadmap §7, percentiles.** Ruled view-shaping (ruling 1).
- **Roadmap §3, adapter shapes.** "On-demand fetch" is `Fetch` (§5).
  The socket rule is unchanged: the data tier is the only place a
  socket opens; `geode-chart` and `geode-timeseries` open nothing.
- **Market-data design §5.2.** `Adapter` gains a third optional
  capability, `fetch()`. Existing adapters return `None`.
- **Foundation design §7.4 budgets.** Unchanged; §1.2 restates the two
  that bind here.

## 4. The series family

### 4.1 Why its own family

Documents publish whole: staging, insert into live, a full copy of the
outgoing generation into the archive. That is O(rows) per publish and a
full copy per generation on disk until retention. A series extends:
widening a range by a month must not rewrite two years of minute bars,
and a tailed point per second must not either. A series also carries
its own time axis, so the generation model is the wrong tool for
as-of. The family is therefore bitemporal and append-only, with no
archive and no generations, and as-of is a filter on `received_at`.

The query path is the same engine and budget either way; the write
path is the difference.

### 4.2 Declaration

```toml
[series]
family = "series"
retention = "30d"     # superseded rows older than this are deleted
history = "5y"        # rows whose ts is older than this are deleted
```

`Family` gains `Series`. A series dataset declares no columns: the
family implies them. `retention` and `history` are the only other legal
keys; anything else is an error diagnostic on the datasets doc.
`retention` and `history` are parsed by the existing `parse_duration`
and default to unbounded.

Sources fill a series dataset the way every source names its dataset,
and several fetch sources may share one:

```toml
[sources.kdb_hist]
adapter = "kdb"
dataset = "series"
[sources.rest_prices]
adapter = "rest"
dataset = "series"
```

A fetch source's adapter-specific keys (`url`, `host`, `port`) ride on
the source table as `topics` does for a subscribed one; `paths`,
`readiness`, `poll_interval` and `pending_timeout` on a fetch source
are a warning, as on a subscribed one. A source whose adapter offers
no `Fetch` but names a series dataset is `Failed` on the discovery
lane at `DataService::open` with the reason, the existing treatment
of an unservable subscribed source.

### 4.3 Validation (`validate_dataset`)

A series dataset must declare no columns, no `key`, no `axes`, no
`grain`. Its implied schema, in this order, is what
`series_columns()` returns and what storage, publish and the query
compiler share (the `document_columns()` rule, restated for this
family):

| column | type | role |
|---|---|---|
| `source` | `utf8` | key, stamped by the ingest from the request |
| `series_id` | `utf8` | key |
| `ts` | `timestamp` | axis |
| `received_at` | `timestamp` | version |
| `value` | `f64` | value |

`ColumnRole` gains `Version`. `DatasetSpec::groupable_columns` answers
none for this family, and no scope reaches it: the series query (§6)
takes no scope at all, so `ScopeSemantics::NotApplicable` still has no
producer.

### 4.4 Storage and append

One table per dataset, `series_<dataset>`, the five columns above,
primary key `(source, series_id, ts, received_at)`. A second table
`series_<dataset>_coverage` holds `(source, series_id, from_ts, to_ts,
received_at)`, one row per fetch.

`append_series(conn, dataset, source, series_id, rows, received_at,
span)` is the one door rows enter by, whatever produced them:

1. Rows are written to the fixed staging table `staging_series` (the
   cold-start handoff's global-name rule applies: it is one table for
   the process, never per source).
2. Rows equal to the current live value for their `ts` are dropped:
   `delete from staging where exists (select 1 from live_view where
   same key and ts and value = staging.value)`. An overlapping refetch
   that returns the same numbers therefore grows nothing.
3. The rest are inserted with the one `received_at`.
4. The coverage row `(source, series_id, span.0, span.1, received_at)`
   is inserted even when nothing was appended, so an empty gap is not
   asked for again this session.

"Live" is a view per dataset: the row with the greatest `received_at`
per `(source, series_id, ts)`, expressed as `arg_max(value,
received_at)` grouped by the three. The query compiler (§6.2) inlines
the same expression under its as-of filter rather than reading the
view, so as-of costs no extra pass.

Every append runs under `geode_core::panic::contained`; a panic marks
the pair `Failed` on the load lane.

### 4.5 As-of

`AsOf::At(t)` selects rows with `received_at <= t` and, for the frame's
as-of, `ts <= t` as well, then takes the latest `received_at` per `ts`.
A corrected value is one more row; the earlier value is what an as-of
before the correction sees. `AsOf::Live` is the view. There is no
generation, so `resolve_generations` is not involved and the as-of
routing table has no arm for this family.

### 4.6 Coverage

The catalog (`CatalogOutcome`) gains series rows: `(dataset, source,
series_id, from_ts, to_ts, rows, latest_received_at, health)` where
the span is the union of the pair's coverage rows. The picker reads
"already loaded" from it and the diagnostics tile lists it. Coverage
subtraction (§5.4) is a pure function over the pair's coverage rows:
`missing(requested, loaded: &[(from, to)]) -> Vec<(from, to)>`,
half-open spans, merged before subtracting.

### 4.7 Retention

The existing sweeper gains a series arm: `retention` deletes rows
that are superseded (a later `received_at` exists for the same
`(source, series_id, ts)`) and older than the window, so the live row
survives whatever its age; `history` deletes rows and coverage whose
`ts`/`to_ts` are older than the window. `SweepReport` gains a count
for each. Neither touches the live row of a `ts` inside `history`.

### 4.8 Health and freshness

The discovery lane carries the adapter's `ConnectionState` exactly as
for a subscribed source. The load lane is keyed by
`"{identity}@{source}"` and carries a fetch that failed or was
refused, with the adapter's message; the next successful fetch for the
pair clears it. A failure is not persisted, so the lane starts `Ok`
for every pair at open. Freshness for a pair is its latest
`received_at`.

### 4.9 Demo database

`--demo`'s database directory (`geode-app::demo::demo_dir`) gains the
series tables under the same `CREATE TABLE IF NOT EXISTS` rule; the
family's column set is fixed, so the reorder hazard the CLAUDE.md
records for measures cannot arise here.

### 4.10 As built (Part 1)

Part 1 (the data tier) is built; §6–§10 are not. Where the code
differs from the sections above, the code is the specification now.

- **Retention runs per pair inside `append_series`, and there is no
  sweeper (amends §4.7).** `store::retention::sweep` gained no series
  arm and `SweepReport` no series counts. `store::series::sweep_pair`
  runs in the append's own transaction with `now = received_at`: the
  one place a series table grows is the one place it is bounded, and a
  sweeper over every pair would have to scan the whole table to find
  the pairs that grew. `SeriesAppended` carries `swept` beside
  `appended`. The two predicates are unchanged from §4.7.
- **A window that cannot be applied is an error diagnostic at load
  (amends §4.2).** `SchemaSpec::from_doc` diagnoses both silent forms
  of a `retention`/`history` declaration — a non-string value
  (`retention = 30`, which `as_str()` alone turns into "absent") and a
  value whose microseconds exceed `i64::MAX` (`parse_duration` accepts
  `y`, so `"300000000y"` parses) — as an error at
  `datasets.<name>.<field>`, leaving the window `None`, so the
  "unrepresentable window sweeps nothing" answer in
  `store::series::cutoff` is now only reachable by a hand-built
  `DatasetSpec`.
- **A fetch is clipped to `history` before coverage is subtracted
  (amends §4.7, and is the consequence of the amendment above).**
  Because the sweep is inside the append, a span older than `history`
  would be inserted and deleted in one breath, its coverage row with
  it, so the next look would ask for the same dead span forever.
  `DataService::fetch` raises `from` to `now - history` first
  (`checked_sub_signed`, and no clip at all if the window does not
  convert or the subtraction leaves the representable range —
  `history` is user-configured and unbounded in magnitude, and not
  clipping is the safe direction), and answers `Ok(0)` without asking
  the source when nothing of the request survives.
- **Health is reported before the asking tile is answered, on every
  path (clarifies §4.8).** The fetch worker's `Failed` arm and the
  runner's `SeriesAppended`/`SeriesFailed` arms all call the load
  report first and send `DataEvent::SeriesFetched` second, so the two
  read the same way wherever an outcome comes from. The load-lane key
  is `"{identity}@{source}"` at all three sites, which is what lets a
  success clear a failure, and both arms close their `Started` with a
  `LoadEnded` exactly as the file and document arms do.
- **A `Fetch` source has no connection state, so a servable one is
  `Ok` on the discovery lane at open (amends §4.8).** `Fetch` exposes
  no `ConnectionState` — there is no long-lived connection to lose —
  so `DataService::open` reports a clean discovery lane once the
  worker spawns, and an unservable one (no adapter in this build, no
  fetch side, or a worker that would not spawn) is `Failed` there.
  That report goes through the same emit closure every other health
  report uses, never a `|_| true`: the tracker commits a transition
  only when its emit says the event was delivered, so a dropped one
  would both hide the `ok` and mark as reported a value nothing saw.
- **`SeriesCatalog` carries `fetches` and the coverage hull, not a row
  count (amends §4.6).** One row per pair, read from the coverage
  table alone — `source`, `identity`, `from`/`to` (the hull of its
  fetched spans), `fetches` (how many coverage rows), and
  `latest_received_at`. Coverage is one row per fetch, so this is
  catalog-sized and honours `build_catalog`'s rule that nothing there
  scans a data table; a row count over the series table would have
  broken it.
- **The identities request is `Request::Identities { source }`
  (amends §5.3/§5.5's `Catalogue` naming).** It asks one fetch source
  to re-answer `Fetch::catalogue`; the answer lands in
  `CatalogSnapshot::identities` as `(source, identities)` pairs. A
  source that cannot enumerate is not a failure — the picker simply
  has no typeahead for it.
- **A `Fetch` shim owes its own deadline (clarifies §5.2).** The
  worker's shutdown drops its sender and joins the thread, with no
  cancellation, so an unbounded vendor call holds app exit for its whole
  duration; the `Fetch` trait doc states the contract, and the demo
  adapter (pure CPU) needs no deadline.
- **`FetchWorker` hands an outcome to a service-built sink, which
  submits to the ingest runner (clarifies §5.4).** The worker owns the
  adapter and nothing else: rows become a `SeriesJob` on the runner's
  series lane, so the append runs on the ingest thread, the one door
  storage is entered by; a failure is the load lane plus the asking
  tile's answer; a catalogue is remembered for the next `catalog`
  read. `take_work` pops documents, then series, then files, so an
  interactive fetch never waits behind a backfill.
- **Coverage subtraction runs on the service thread, through the
  service's own reader connection (clarifies §5.4).** `coverage` +
  `missing_spans` are read and computed in `DataService::fetch`
  itself, not in the worker and not in the pool: `append_series`
  commits rows and coverage in one transaction, so a span this reader
  sees as covered is a span whose rows are queryable.

## 5. The fetch adapter shape

### 5.1 Config

§4.2. `SourceSpec` gains nothing new for fetch beyond the adapter's own
keys; whether a source is fetched is answered by its adapter offering
`fetch()`, not by a config key.

### 5.2 Traits

In `geode_data::adapter`:

```rust
pub struct FetchRequest {
    pub identity: String,
    pub from: DateTime<Utc>,   // half-open: from <= ts < to
    pub to: DateTime<Utc>,
}

/// Struct-of-arrays, sorted by `ts`, equal lengths.
pub struct SeriesRows { pub ts: Vec<DateTime<Utc>>, pub value: Vec<f64> }

pub trait Fetch: Send {
    /// History for one identity over a span at the source's native
    /// grain. Called on the source's fetch thread; blocking is fine.
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError>;
    /// Identities this source can name, for typeahead. `None` when the
    /// source cannot enumerate (a REST endpoint).
    fn catalogue(&mut self) -> Option<Vec<String>>;
}

pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn subscription(&self) -> Option<Box<dyn Subscription>>;
    fn egress(&self) -> Option<Box<dyn Egress>>;
    fn fetch(&self) -> Option<Box<dyn Fetch>>;   // new; default None
}
```

That is the whole surface a KDB or REST shim implements. Unsorted or
unequal-length rows are an `AdapterError` at the pipeline's validation
step, never a panic.

### 5.3 The request

```rust
Request::Fetch(FetchParams { key: QueryKey, source: String, identity: String,
                             from: DateTime<Utc>, to: DateTime<Utc> })
Request::Catalogue { source: String }
```

`DataHandle::fetch(params) -> bool` and `DataHandle::catalogue(source)
-> bool`, refusing at `REQUEST_BOUND` as every request does.

### 5.4 The pipeline

`DataService::open` builds one `Fetch` and one fetch thread per source
whose adapter offers it, with a bounded queue. On `Request::Fetch` the
service thread:

1. Resolves the source; an unknown source or one without `Fetch` is a
   `SeriesFetched` with `Err(reason)` at once.
2. Reads the pair's coverage rows and computes the missing spans
   (§4.6). None missing: `SeriesFetched` with `Ok(0)` at once, so the
   tile requeries without waiting.
3. Queues one job per missing span on the source's fetch thread.

The fetch thread, per job, under `contained`:

1. `fetch()`; an `Err` is a load-lane `Failed` for the pair
   (`report_load_and_emit`) and a `SeriesFetched` with `Err`.
2. Validates the rows (sorted, equal length, finite values; a
   non-finite value is dropped with a warning log line).
3. `append_series` (§4.4) with `received_at = now`.
4. Clears the pair's load-lane failure and emits `SeriesFetched`.

One event carries both outcomes, keyed by the pair, never by the asking
tile:

```rust
DataEvent::SeriesFetched { source: String, identity: String, result: Result<u64, String> }
```

`Ok(appended)` after step 4, `Err(reason)` from step 1 of either
thread. The bridge bumps nothing in the frame (a series fetch is not a
`Published` file and must not requery every blotter) and routes it to
every visible occupant as `Delivery::SeriesFetched { source, identity,
result }`. A tile holding the pair marks the slot and, on `Ok`,
requeries; a tile that does not hold it ignores the arm. Keying by
pair is deliberate: two tiles holding `SPX.close@kdb_hist` both learn
the outcome of the one fetch that answered them both.

Two tiles asking for the same pair and span produce two jobs; the
second finds nothing missing at step 2 if the first has landed, and
otherwise appends nothing at §4.4 step 2. No dedupe of in-flight jobs
is built; the cost of the second fetch is bounded and the logic to
avoid it is not.

### 5.5 Catalogue

`Request::Catalogue` runs `catalogue()` on the source's fetch thread
and stores the result in the service; `DataService::open` queues one
per fetch source. The catalog outcome gains `catalogues: Vec<(source,
Vec<String>)>`, so the picker reads identities from the shell's
`Diagnostics` entity through `request_catalog()` plus `cx.notify()`,
the market-data panel's route, with no new shell door.

### 5.6 The demo adapter

`geode_app::demo_series`: an in-process `Fetch` with no thread of its
own, a seeded deterministic generator (seed 42, as `--demo`'s data).
Native grain one minute over 09:30–16:00 New York on weekdays, no
holiday list; 24 identities (`SPX.close`, `SPX.vol_1m`, `SPX.vol_3m`,
`SX5E.close`, `NKY.close`, `VIX`, `V2X`, `SPX.skew_3m`, …) as
geometric random walks with a per-identity level, drift and vol, and a
weekly seasonality on the vol identities so a ratio has shape. A
request for an unknown identity is `AdapterError("unknown identity")`.
Generation is pure and per-span, so the same span always returns the
same rows, which is what §4.4 step 2's "refetch grows nothing" test
depends on.

Two demo sources are registered under `--demo`: `demo_kdb` with a
catalogue, and `demo_rest` without one, sharing the generator; the
`--demo` config sets `[timeseries] default_source = "demo_kdb"` and a
`[series]` dataset with `retention = "7d"`.

## 6. The series query

### 6.1 Request

```rust
pub struct SeriesParams {
    pub key: QueryKey, pub tag: u64,
    pub dataset: String,
    pub range: (DateTime<Utc>, DateTime<Utc>),   // half-open
    pub window: (DateTime<Utc>, DateTime<Utc>),  // visible span, for stats
    pub as_of: AsOf,
    pub frequency: Frequency,
    pub series: Vec<SeriesSpec>,
    pub percentiles: Vec<f64>,     // fractions in (0,1); empty = off
    pub bins: Option<u32>,         // None = density off; Some(n), 4..=200
}
pub struct SeriesSpec { pub slot: u8, pub kind: SlotKind }
pub enum SlotKind {
    Source { source: String, identity: String, rule: BucketRule },
    Expr(Expr),                    // §7; references are slots
}
pub enum Frequency { M1, M5, M15, H1, D1, W1 }
pub enum BucketRule { Last, First, Mean, Min, Max }
```

`Frequency`, `BucketRule`, `SeriesSpec` and `Expr` live in
`geode_core::series` because the module builds them and the compiler
consumes them.

`DataHandle::series(params) -> bool`. `Request::Series` runs on the
query pool with `Request::Query`'s cancellation and coalescing: one in
flight per key, a newer tag supersedes.

### 6.2 Compilation (`geode_data::query::series`)

Pure, string-in string-out, like `compile_document`. One CTE per source
slot:

```sql
s1 as (
  select time_bucket(interval '1 day', ts) as b, arg_max(v, ts) as v   -- rule last
  from (
    select ts, arg_max(value, received_at) as v
    from series_x
    where source = ? and series_id = ? and ts >= ? and ts < ?
      [and received_at <= ?] [and ts <= ?]                              -- as-of
    group by ts
  ) group by b
)
```

Rules: `last` is `arg_max(v, ts)`, `first` is `arg_min(v, ts)`,
`mean`/`min`/`max` are themselves. An expression slot is a select over
the **inner** join of its operands' CTEs on `b` (ruling 11), with the
AST lowered to SQL arithmetic; a division emits `case when denominator
= 0 then null else … end`. The points result is one wide row set:
`buckets` is the `union` of every source CTE's `b`, and the select is
`buckets left join` each slot CTE on `b`, ordered by `b`, so a bucket
present in any slot is a row and a slot without it is `NULL`.

Stats are a second and third statement in the same batch, each over
the slot CTEs filtered to `window`: `quantile_cont(v, [f…])` per slot,
and `width_bucket(v, lo, hi, n)` counts per slot with `lo`/`hi` the
slot's own `min`/`max` over the window. A slot with fewer than two
distinct values yields no bins.

Every parameter is a bound value; identities are never interpolated
into the text. SQL-text tests pin one shape per rule, the as-of forms,
an expression with a gap, the zero denominator, percentiles and bins.

### 6.3 Cap

`frequency.buckets_in(range) > 500_000` is refused on the service
thread before compilation, as a `SeriesOutcome` whose `result` is
`Err("1m over 3y is 1,170,000 points; the cap is 500,000")`. The
tile paints it as a notice and keeps the last good model.

### 6.4 Outcome and delivery

```rust
pub struct SeriesOutcome {
    pub key: QueryKey, pub tag: u64,
    pub result: Result<SeriesResult, String>,
}
pub struct SeriesResult {
    pub buckets: Vec<i64>,                  // micros since epoch, ascending
    pub slots: Vec<SlotResult>,             // in request order
}
pub struct SlotResult {
    pub slot: u8,
    pub values: Vec<f64>,                   // buckets.len(); NaN = no bucket
    pub percentiles: Vec<(f64, f64)>,       // (fraction, value)
    pub bins: Vec<(f64, f64, u32)>,         // (lo, hi, count)
    pub provenance: SlotProvenance,         // loaded span, latest received_at, load-lane health; None for an expression
}
```

Struct-of-arrays, `NaN` for a gap, no `Snapshot`: there is no tree,
grouping or attribution, and the chart wants arrays. Delivered as
`Delivery::Series(SeriesOutcome)`; every occupant's exhaustive `match`
names the site.

### 6.5 Lifecycle in the tile

The tile requeries on every model change that reaches the query
(slots, rules, range, frequency, window, percentiles, bins), on
`SeriesFetched` with `Ok` for a pair it holds, and on the frame's `as_of` counter
alone under the flip barrier: `follows_changed` compares `as_of` and
nothing else, `flip` excluded as everywhere. A delivery is staged and
promoted per the barrier, as the market-data panel's is; a delivery
whose tag is stale is dropped. `set_visible(false)` cancels in flight.

### 6.6 As built (Part 2)

Part 2 (the series query and the expression language) is built; §8–§10
are not. Where the code differs from §6 and §7 above, the code is the
specification now.

- **The pinned DuckDB has no `width_bucket`, so the bin index is that
  function's definition written out (amends §6.2).** `Catalog Error:
  Scalar Function with name width_bucket does not exist!` is what the
  spec's form earns. The statement now reads
  `cast(least(floor((w.v - m.lo) / (m.hi - m.lo) * k) + 1, k) as
  bigint) as k`, over `w` (the window's non-NULL values) and `m` (its
  `min`/`max`), with `where m.lo < m.hi` making the division safe and
  standing in for "fewer than two distinct values yields no bins". The
  `least` is load-bearing and not decoration: `hi` itself lands in bin
  `k + 1`, which `run_series`'s `1..=k` range check would silently
  DISCARD — the histogram short by however many rows sit exactly on the
  maximum, with no error anywhere. The `cast` pins the column to BIGINT
  whatever `floor` returns, because the reader binds it as one.
- **`SeriesParams`, `SeriesOutcome` and every type around them live in
  `geode_core::series`, not `geode_core::query` (clarifies §6.1).**
  Same reason `query.rs` gives for its own contents: the module builds
  these and the compiler consumes them, and `geode-shell` and
  `geode-data` may never depend on each other. `expr::Expr` is
  `Ast<u8>` — the parsed tree is `Ast<RefName>` and `resolve` is the
  one door between them.
- **Both carry `submitted: Instant` (amends §6.1, §6.4).** The view
  path grew it for the §7.1 timing readout and the series path is
  measured the same way; it is echoed back untouched, like `tag`.
- **`SlotProvenance` is filled from two places (clarifies §6.4).**
  `run_series` fills `loaded` (the coverage hull, `min(from_ts)` /
  `max(to_ts)`) and `latest_received_at` (`max(received_at)`) from the
  per-source-slot coverage statement, and leaves `health` `None`; the
  service's result sink fills `health` from the tracker's load lane.
  An expression slot has no coverage statement and keeps all three
  `None`.
- **The coverage statement ignores as-of — a known gap, left for Part 4
  (amends §6.4).** It reads the coverage table for the pair with no
  time bound, so a historical as-of reports LIVE freshness: the hull
  and the newest fetch as they stand now, not as they stood then. The
  honest fix is a `received_at <= t` bound on the coverage read, but
  what a tile should *show* under an as-of (the span it is painting, or
  the span the pair holds today) is a display question no section here
  answers — §9.3 and §9.5 spell out health, not freshness — so nothing
  is guessed at from the data tier. Part 4 decides it.
- **Health reaches the outcome through `RequestKind::Series { pairs }`,
  matched BY SLOT NUMBER (clarifies §6.4).** The request carries every
  SOURCE slot's `(slot, source, identity)`; the result sink looks each
  one's `"{identity}@{source}"` up in the load lane and files it on the
  slot with that number. `SeriesResult::slots` holds every slot in
  request order, expression slots included, while `pairs` holds only
  the source ones, so the two lists differ in length the moment a
  request has an expression — a positional zip marks the wrong pane in
  both directions, and `health_is_attached_by_slot_number_not_position`
  is the test that would see it.
- **Strings and timestamps are bound; three kinds of number are
  formatted into the text (amends §6.2's "every parameter is a bound
  value").** Identities, sources and every instant are bound values and
  never text, as §6.2 says. The percentile fractions (`{f:?}`), the bin
  count (`{k}`) and an expression's numeric literals (`{x:?}` in
  `lower`) are not: they are written into the statement. Each is checked
  before it gets there — `validate` refuses a fraction outside `(0, 1)`
  and a bin count outside `MIN_BINS..=MAX_BINS`, and `lower` refuses a
  non-finite literal, which is the whole reason that refusal exists.
  Beyond those, only table names, the `time_bucket` interval, the
  aggregates and `s{n}` are text.

- **The pool carries a second payload kind (clarifies §6.1).**
  `Work::{Query(CompiledQuery), Series(Box<SeriesPlan>)}` and
  `Payload::{Snapshot, Series}`; the pool's coalescing, interruption
  and containment never look inside either. `DataService::series`
  checks the cap, compiles, and submits a `QueryRequest` with an empty
  grouping and a default `Provenance` — a series has no tree and its
  provenance is per slot.
- **The result sink reads the health tracker under the pool's queue
  lock (a lock-order rule, new).** The order is pool queue lock →
  tracker lock, never the reverse, and nothing reachable from a
  `report_*_and_emit` emit closure may touch the pool.
  `HealthTracker::load_lane` is a read that neither stamps nor offers,
  so asking it per slot cannot disturb the transition bookkeeping the
  two report doors own.
- **The cap is refused on the service thread and becomes the asking
  key's own outcome on the handle (clarifies §6.3).**
  `DataService::series` returns `Err(StoreError::Series(..))` before
  `compile_series` runs; `DataHandle::series` is `-> bool` as §6.1
  says, and the serve loop turns that `Err` — a cap refusal or a
  compile refusal alike — into a `DataEvent::Series` whose `result` is
  `Err(text)`, so the asking tile always hears back.
- **`Delivery::key()` is `Option<QueryKey>`, and a key-less delivery is
  broadcast to the VISIBLE occupants (amends §5.4, §6.4).**
  `Delivery::Series` answers `Some(outcome.key)` and is routed to the
  tile whose id it is, dropped if that tile is gone.
  `Delivery::SeriesFetched` answers `None`, and `ShellView::deliver`
  hands every occupant of a tile on screen (`visible_tile_keys` — the
  same visible set the flip barrier waits on, placeholders filtered,
  visible docks covered) its own copy, since `Delivery` is not `Clone`.
  Hidden tiles are skipped on purpose: they hold no subscription and
  requery on `set_visible(true)`.
- **The identity grammar is spelled out, and an identity outside it is
  referenced by its handle (clarifies §7).** An identity is
  `[A-Za-z_][A-Za-z0-9_.]*` and a source is `@[A-Za-z0-9_-]+`, so
  `SPX.close@kdb_hist` parses and a REST-path identity
  (`/v1/px?sym=SPX`) does not — such a pair is referenced by its slot
  handle, `s3`, which every loaded slot has. A handle is `s` followed
  by digits and NOTHING else, so `spx_1y` and `s1x` are identities, and
  `s999` (past `u8`) is an identity rather than an error. A `(`
  immediately after a word is a function call and earns the
  "arithmetic only" message at the point a trader would cross the
  boundary; a second `@` earns it too.
- **An expression is bounded twice: `MAX_DEPTH = 64` levels of nesting
  and `MAX_TOKENS = 256` tokens (new, amends §7).** `Parser::factor`
  recurses on `-` and `(`, and a pasted wall of parentheses would
  overflow the stack — an abort, not a panic, and nothing can contain
  it; `MAX_DEPTH` refuses that with the message about nesting, and
  sixty levels still parse. The depth cap alone is NOT the whole bound
  (final review, Important): `expr`/`term` fold left-deep
  *iteratively*, so `1 + 1 + …` builds a tree as deep as it is long
  while nesting nothing at all. The token bound, checked in `parse`
  right after tokenizing, is what caps the tree's node count and hence
  every recursion OVER the tree — `Ast::resolve`, `Expr::collect_slots`,
  the compiler's `lower` and the `Box` drop glue. The two together are
  the guarantee; a chain past 256 tokens earns "expression is too long
  (more than 256 tokens)".
- **A non-finite literal is refused by the compiler (new).** Rust's own
  `str::parse` answers `Ok(inf)` on overflow rather than an error, so a
  400-digit literal reaches `lower` as `Ast::Num(inf)` and `{x:?}`
  would format the bare word `inf` into the SQL text. `lower` refuses
  it with "literal … is not a finite number".
- **The resolution rules of §7 are the module's, and are not built
  here.** The parser resolves what it is handed through a caller's
  closure (`Ast::resolve`, `Err` naming the first reference that
  missed) and `expression_order` answers the operands-first order with
  `Err(slot)` for a cycle; "a bare identity by the default source",
  ambiguity, and removing an operand's dependants all belong to Part 4
  and `[timeseries] default_source`, which does not exist yet. The
  compiler's own `validate` refuses an expression with no references,
  a reference to a slot the request lacks, and a cycle.
- **The bench is `benches/series_query.rs` (amends §11.3's
  `benches/series.rs`).** It reads beside `append_series` that way. The
  timed half is the round trip a tile pays — `DataService::series` plus
  the wait for its `DataEvent::Series` — over four identities of
  one-minute bars for a year (1,000,000 rows): **3.64 ms** for one slot
  at `1d` over the year (that slot's 250,000 rows into the ~300 daily
  buckets its sessions touch — each 1,000-minute session starts at 14:30
  UTC and spills into the next UTC day), **9.56 ms** for four slots plus an `s1 / s2` expression
  (the whole million), **14.5 ms** for two slots at `1m` over a month
  with three percentiles and 40 bins. All three are inside §7.1's 50 ms, the
  widest by a factor of three. **Known gap:** every stats statement
  carries the same CTE prefix as the points statement and so re-runs
  the bucketing — a request with percentiles and density on runs
  `1 + 2k` bucketing passes for `k` slots. A single `grouping sets`
  statement is where that goes if a desk ever asks for stats over a
  range where it bites; `docs/perf.md` has the conditions and the
  measurement.

## 7. The expression language

Grammar (in `geode_core::series::expr`, a hand-written recursive
descent parser, pure):

```
expr   := term (('+' | '-') term)*
term   := factor (('*' | '/') factor)*
factor := '-' factor | '(' expr ')' | number | ref
ref    := handle | identity ('@' source)?
handle := 's' digit+
```

- A `number` is a decimal literal.
- A `ref` is resolved by the module against the tile's loaded slots
  before the AST is sent: a handle by slot number, `identity@source`
  by exact pair, a bare `identity` by the default source, or, when
  the default source does not hold it and exactly one loaded slot has
  that identity, that slot. An unresolved or ambiguous reference is a
  parse error naming it, shown in the field; nothing is sent.
- An expression may reference another expression's slot; a cycle is a
  parse error. The compiler lowers references transitively.
- The parsed `Expr` carries slot numbers only; identities never reach
  the compiler as text (§6.2).
- An expression slot is a series like any other: colour, axis,
  visibility, a chip. Its bucket rule is its operands'. Removing an
  operand removes every expression that references it, transitively,
  with one notice naming them.

Tokens that are not in the grammar (`^`, a function name, `%`) are a
parse error saying "arithmetic only: + - * / and parentheses", so the
boundary is stated at the point a trader would cross it.

Built in Part 2, with §6.6 the record: the parser, `Ast::resolve` and
`expression_order` are in `geode_core::series::expr`; the identity
grammar, the handle rule for an identity outside it, the `MAX_DEPTH`
nesting cap, the `MAX_TOKENS` length bound and the non-finite literal
refusal are all there. The
*resolution* rules above — a bare identity by the default source,
ambiguity, removing an operand's dependants — are the module's and
belong to Part 4.

## 8. `geode-chart`

A new crate, `bench = false`, depending on `gpui`, `gpui-component`
and `geode-core` (for the colour floor). It knows nothing of series,
sources or the data tier, and it does not depend on the shell: the
rem-scale door is `geode_shell::shell::scale`, so the element takes its
rem size as a parameter per paint (`ChartElement::new(model, view,
rem_px)`) and the module reads `window.rem_size()` and passes it.

Design-pixel constants, all at the `FontSize::Medium` rem:
`DENSITY_STRIP` = 80, `TICK_GAP` = 64, `DASH` = 4, `GAP` = 3,
`AXIS_WIDTH` = 44.

### 8.1 Core (`geode_chart::core`, no gpui)

- `LinearScale`: domain `(f64, f64)` to pixel range and back; nice
  ticks by the usual 1-2-5 stepping, count from the axis height.
- `TimeScale`, two modes (§8.2).
- `Layout::solve(bounds, options) -> Layout`: one or two panes. A
  pane is a plot rect, a left axis rect and a right axis rect (each
  only when a visible slot uses that side), and a density strip rect
  (only when density is on; width `DENSITY_STRIP` design px). The
  lower pane exists only while a visible slot uses `BottomLeft` or
  `BottomRight` (ruling 12); the upper pane takes `split` of the
  height (default 0.7, clamped to 0.2..=0.8) and a `PANE_GAP` = 6
  design px separates them. One x-axis rect under the lowest pane,
  shared: both panes use the same `TimeScale` and `View`. Lengths are
  design pixels resolved through the caller's rem.
- `decimate(x: &[f32], y: &[f64], columns: usize, out: &mut Vec<Point>)`:
  min-max per pixel column into a reused buffer, breaking at `NaN`, so
  no column's extreme is lost and a 500,000-point series is at most
  two points per column.
- `View`: the visible window (an index window under `Session`, a time
  window under `Continuous`) with `pan(fraction)`, `zoom(factor,
  about)`, `reset()`, clamped to the loaded range.
- `Crosshair::at(cursor_x, scale, buckets) -> Option<usize>`: the
  nearest bucket; the readout reads each visible slot's value there.
- `Palette::default_for(slot, theme_chart: [Hsla; 5], background)`:
  cycles the theme's five chart colours, each floored to 3:1 against
  `background` through `geode_core::colour::readable_on`. A slot may
  instead name a `[colours]` entry; the module resolves it and hands
  the element an `Hsla`.

Everything is `Copy` or borrows; nothing here allocates per frame
except `decimate`'s `out`, which the element owns and reuses.

### 8.2 The session axis

`TimeScale::Session { buckets: &[i64] }` maps a bucket index to x, so
any span with no bucket has no width: weekends, holidays, overnight
and half-days collapse from the data, with no calendar. Ticks fall on
the first bucket at which the chosen unit changes (year, month, day,
hour, minute), the unit chosen so ticks sit at least `TICK_GAP` px
apart; each tick's label is the unit's format (`Sep 26`, `14 Sep`,
`10:00`). `TimeScale::Continuous { from, to }` is wall clock with the
same unit chooser over elapsed time. `:axis session|time` switches;
default `session`. A true outage collapses under `session`; that is
the norm and the price.

### 8.3 Element (`geode_chart::element`)

One `ChartElement` implementing gpui-component's `Plot` trait, which
buys crosshair and tooltip plumbing (`tooltip_state`, `CrossLine`,
`Tooltip`) and `PathCache` for the line paths. Inputs: an
`Arc<ChartModel>` (buckets, per-slot values, styles, stats, the axis
mode), the `View`, the rem size and an `ElementId`.

Paint order, per pane: `Grid`, then the pane's `PlotAxis`es with
ticks from its scales, then per visible slot on that pane the
decimated polyline through `PathCache` keyed on `(model version, view,
bounds)`, then that slot's percentile lines as dashed segment paths
(gpui's `PathBuilder` has no dash style; `DASH` design px on, `GAP`
off, one path per line), then the pane's density bars as `paint_quad`
(at most `bins × slots`, a few hundred, under the spike's 5,000-quad
cliff — as built, capped at `MAX_DENSITY_QUADS` = 2,000 per frame,
§8.5). Then, once, the shared x-axis under the lowest pane and the
crosshair, which spans both panes at one x with the readout listing
every visible slot from either pane, through the component's
tooltip. Labels `p5 p50 p95` are painted at the right end of each
line in the slot's colour through `prepaint`'s child elements. A
slot's percentiles and bins are drawn in its own pane against its
own axis.

A frame with an unchanged key pushes cached paths and rebuilds
nothing on the data path (as built: one `Path` clone per painted path
per frame is the pinned `paint_path` API's price, §8.5); the cache
invalidates on model version, view or bounds.

### 8.4 Budget

A criterion bench, `geode-chart/benches/decimate.rs`: 500,000 points
into 1,600 columns, target under 2 ms including the path rebuild.
Numbers into `docs/perf.md`.

### 8.5 As built (Part 3)

Part 3 (the `geode-chart` crate) is built; §9–§10 are not. Where the
code differs from §8 above, the code is the specification now. The
crate depends on `geode-core`, `gpui`, `gpui-component` and `chrono`
and nothing else — `geode-shell` appears under `[dev-dependencies]`
alone, for the bundled-theme sweep.

Built as written: `LinearScale` (1-2-5 nice ticks, `DOMAIN_PAD` either
side of a domain), the two `TimeScale` modes, `Layout::solve` with its
per-side axis columns, density strip, `PANE_GAP` and clamped `split`,
`decimate`'s min-max per pixel column with a `Point::BREAK` at a `NaN`
run, `View` with clamped pan/zoom/reset, the floored `Palette`, one
`ChartElement` implementing gpui-component's `Plot` with cached paths,
dashed percentile lines, density quads, one shared x axis and the
crosshair readout, the criterion bench and the example window.

- **`Crosshair::at(cursor_x, scale, view, plot)` replaces §8.1's
  `(cursor_x, scale, buckets)`.** The nearest bucket is a question
  about PIXELS, so it needs the same three things `x_of` needs — the
  scale, the visible window and the rect the window is mapped into —
  and the buckets reach it through the scale. It binary-searches the
  first visible centre at or past the cursor and compares with its
  predecessor.
- **Paint is batched BY KIND within a pane, not per slot (amends
  §8.3).** §8.3 reads "per visible slot on that pane the polyline,
  then that slot's percentile lines, then the bars"; the element
  paints the grid, the axes, then ALL the polylines, ALL the
  percentile paths, one `PlotLabel` carrying every tag, and then all
  the bars. That is two cache `update`s and one label batch per pane
  instead of three per slot, at the price of a z-order where every
  line sits under every percentile rule — deliberate, and the reason
  a percentile reads as chrome over the data rather than as part of
  one slot.

- **`Palette::from_theme(chart, background, foreground)` replaces
  §8.1's `default_for(slot, theme_chart, background)`.** The floor is
  `geode_core::colour::readable_on`, which needs the direction to move
  a faint colour in; the cycling is `Palette::colour(index)`. The
  sweep (`every_bundled_themes_palette_is_readable`, 220 checks over
  44 themes, no exception list) passes with no theme needing a manual
  exception.
- **Two constants join §8's list: `X_AXIS_HEIGHT` = 18 and
  `Y_TICK_GAP` = 40 design px.** The first is gpui-component's own
  `AXIS_GAP`, so our x labels sit where the component's charts put
  theirs; the second is the minimum vertical distance between two y
  ticks, which is how a pane's tick count is derived from its height.
  `DESIGN_REM` = 12 is a literal mirroring `geode_shell::shell::scale`
  (the crate must not depend on the shell) and is pinned to that value
  by a test; every length resolves through `core::design_px`.
- **The tick chooser is four explicit rules with `MIN_TICKS` = 3, plus
  a resolution floor (amends §8.2's "at least `TICK_GAP` apart").**
  §8.2's single rule picks `Day` for a day of minute bars — two
  candidates 380 px apart — and paints two ticks where a trader wants
  hours. The chooser now takes, in order: (1) the FINEST unit with
  `MIN_TICKS` candidates whose minimum gap already clears
  `tick_gap_px`, unthinned; (2) else the COARSEST unit that still
  keeps `MIN_TICKS` once thinned by `k = ceil(tick_gap / min_gap)`;
  (3) else the FINEST unit with two candidates, thinned; (4) else one
  `Day`-labelled tick on the first visible bucket. Emission keeps an
  `x <= last_x` guard so x strictly increases however coarse the plot.
  Above all four sits `resolution_floor`: no unit FINER than the
  data's own step is offered at all, because on daily sessions every
  bucket is also a new minute and a new hour, so the three units offer
  the SAME candidates and only the coarsest one's label names what
  changed. The floor is the coarsest unit whose length the step fills
  (`step_us` under `Continuous`, the smallest positive delta between
  consecutive visible buckets under `Session`) — 5-minute bars still
  floor at `Minute`, so a zoomed view is not left tickless. Under
  `Continuous` a unit with more than `MAX_CANDIDATES` = 4,096
  boundaries in the view is skipped as unfittable.
- **The `Axis` vocabulary lives here, in `geode_chart::core::axis`
  (clarifies §9.2).** `Axis::{Left, Right, BottomLeft, BottomRight}`
  with `pane()`/`side()`/`next()`/`prev()`/`as_str()`/`letter()`/
  `parse`, plus `Pane`, `Side` and `AxisMode::{Session, Continuous}`
  (whose wire spellings are `session`/`time`, §9.9's `:axis`), all
  re-exported from the crate root. §9.2's module imports them rather
  than declaring its own: the element dispatches on them, so a second
  copy would be two vocabularies for one thing.
- **Percentile tags are painted through `PlotLabel` in `paint`, not
  `prepaint`'s child elements (amends §8.3).** One `PlotLabel` per
  pane carrying every visible slot's tags, right-aligned at
  `plot.right() - TAG_INSET` (width-independent, so `p99.5` does not
  overflow the fixed allowance §8.3 assumed).
- **Each pane's data painting is CLIPPED to its own rect
  (`Window::with_content_mask`).** The element's own mask is the whole
  element, so without this a polyline paints over the left y-axis
  column: a path is built from the visible buckets' CENTRES, and the
  first visible bucket's centre can sit up to half a bucket left of
  `view.lo` — at the zoom floor (`View::MIN_SPAN` = 2 buckets) a
  quarter of the plot's width, not a hairline. One mask per pane
  covers the polylines, the percentile paths and the tag `PlotLabel`
  (the plot rect); a second covers the density bars (the STRIP rect,
  which is a column beside the plot, not inside it, so the plot's own
  mask would erase them). `with_content_mask` intersects with the mask
  in force, so it can only ever narrow. This is unpinned by the
  harness: a content mask is not observable in a `TestAppContext`.
- **An out-of-pane percentile line is SKIPPED, line and tag together.**
  A percentile is computed over the QUERY window while its pane's
  domain comes from the VISIBLE slice, so a zoom into a quiet stretch
  can put p5 or p95 outside the pane. The mask above would now clip
  such a line, but the skip stands on its own and stays: a level the
  pane's domain does not contain has no business being BUILT, and the
  skip happens BEFORE the cache `get`, which is what makes "never
  built" mean "never painted" and is the half the harness can see.
  Clamping instead would park the line on the pane's edge and read as
  a real level at that value. The density bars keep their clamp: a
  clipped bar still reads as "the tail continues past here"; a
  horizontal rule at a value does not. A percentile TAG whose lift
  would leave the pane is placed below its line (`TAG_DROP` = 2)
  instead of above (`TAG_LIFT` = 11) — at the top of the lower pane it
  used to print into `PANE_GAP` and the upper pane, and under the mask
  it would simply be cut in half.
- **At most `MAX_DENSITY_QUADS` = 2,000 density bars per FRAME.** A bar
  is one uncached `paint_quad` and nothing in the model bounds the
  product: `geode_core::series::MAX_BINS` is 200 and a tile may hold
  many slots, so nine density slots are ~1,800 quads every frame
  against a spike that disqualified per-cell quads past about 5,000
  and measured 10,000 at 42 ms
  (`docs/superpowers/spikes/2026-08-29-gpui-chart-rendering-spike.md`)
  — §8.3's "at most `bins × slots`, a few hundred" was wrong by an
  order. The element counts the bars it paints across BOTH panes and
  stops at the bound, per pane in slot order, so a tile with more
  density slots than fit paints the first ones and drops the rest of
  that frame. **Part 4 owns the slot count**: the bound is the render
  thread's backstop, not a presentation rule, and a module that lets a
  trader turn density on for a dozen slots should say so in its own
  vocabulary rather than let bars silently vanish.
  `element::density_quads()` is the thread-local counter, beside
  `rebuilds()`/`chrome_rebuilds()`, and
  `a_frame_paints_at_most_the_density_bound` pins both directions.
- **`ChartElement::new`'s `ElementId` must be unique among the charts
  in one window.** Every scrap of cross-frame state hangs off it — the
  reused `Buffers` and both sets of `PathCaches` live under the
  element's `GlobalElementId`, which `Plot` takes straight from this id
  — so two charts sharing an id share one chrome key (thrashing between
  their models every frame) and one set of path caches, where two
  models that agree on `(version, slot.number, pane, view, plot rect)`
  serve each other's paths and paint the wrong line with every
  assertion still green. **Part 4 passes the tile's own `TileId`**,
  never a literal; the example and the test hosts pass `"chart"`
  because each has exactly one.
- **Two caches. The paths' keys are `(model.version, slot.number,
  pane, view.key())` plus the plot rect's `x`, `y`, `w`, `h` — and for
  a percentile `(model.version, slot.number, percentile index)` plus
  its own `y` and the plot's `x` and `w`. The chrome's is
  `(model.version, view.key(), offset_secs, buckets.len())` plus the
  bounds width, the bounds height and the rem.** Neither PATH key
  carries a rem or a bounds term of its own: the rem reaches them
  through the plot rect the layout solved with it. Neither key of
  either kind carries `axis_mode` or `step_us`, which is the whole of
  the version-bump contract below. The paths sit behind
  gpui-component's own `PathCache`; the chrome — the four sides'
  `LinearScale`s, each a scan of every visible value on that side,
  their y ticks and the tick LABELS, and the x ticks — sits behind
  `chrome_key` in the element's own `Buffers`. Without the second one a steady frame re-ran up to four
  full scans of every visible value (≈2M comparisons at the
  500,000-point cap), computed each pane's y ticks three times and
  allocated a `String` per y tick per axis, all invisible to the path
  counter. `element::rebuilds()` counts path misses and
  `element::chrome_rebuilds()` chrome derivations; both are
  thread-local `Cell`s, not process-wide atomics, because two
  `#[gpui::test]`s paint on their own threads in parallel and an
  atomic folds one's frames into the other's delta at random.
  `an_unchanged_frame_rebuilds_nothing_and_a_moved_view_rebuilds` is
  the test that pins both.
- **An unchanged frame REBUILDS nothing; it does not allocate
  nothing.** What the two caches buy is that no work proportional to
  the data runs again: no `xs` refill, no decimation, no
  tessellation, no `core::time::ticks`, no scan of the values, no tick
  label. `rebuilds()`/`chrome_rebuilds()` pin exactly that claim and
  nothing wider. Two allocation classes remain on every frame, and
  both are the pinned API's price. (1) `Window::paint_path` takes its
  path BY VALUE, so `PathCache::get` clones and translates the cached
  path on every call, hit or miss (registry `plot/path_cache.rs`) —
  one vertex `Vec` and one walk of its vertices per painted path,
  bounded by the DECIMATED point count (two per pixel column, so
  ~3,200 for a maximised plot) rather than by the 500,000-point cap.
  There is no fix short of forking the component. (2) The chrome the
  component's own painters take: `Grid` takes its lines as `Vec`s,
  `PlotAxis` and `PlotLabel` each collect a small `Vec`, and the
  tooltip builds `SharedString`s while the cursor is over the plot —
  each bounded by the TICK count, and the same cost every chart
  gpui-component ships pays.
- **`ChartModel` carries `version`, `step_us`, `offset_secs` and
  per-slot `percentile_labels` (clarifies §8.3).** `version` is the
  first component of every cache key, and **the module must bump it on
  ANY change to the model** — including a flip of `axis_mode` or
  `step_us`, which the keys read THROUGH the model but do not carry as
  terms of their own. `step_us` is the display bucket's width (the
  frequency) and `offset_secs` the trader's local offset, applied to
  every displayed time (Phase 4a's ruling); `percentile_labels` is
  parallel to `percentiles` and pre-formatted by
  `ChartModel::percentile_label`, so nothing formats a tag per frame.
  `layout_options` reserves an axis column per side a VISIBLE slot
  uses and the density strip only while a visible slot HAS bins.
- **Registry corrections worth knowing (they are not in §8.3).**
  `PlotAxis::new()` leaves `y_axis` FALSE, so a y axis needs
  `.y_axis(true)` or the stroke it is handed paints nothing;
  `Grid::x`/`y` take a `Vec`, and `Grid::paint(&bounds, window)` takes
  no `cx` (unlike `PlotAxis::paint`); there is no
  `From<(&'static str, ElementId)>`, so the per-pane cache keys are
  `(&'static str, usize)`; `Buffers` and `PathCaches` are both
  entities and their `update`s cannot nest, so the buffers are
  `mem::take`n once at the top of `paint` and put back once at the
  bottom.
- **Known limits, none of them pinned.** `MAX_PERCENTILES` = 8
  silently truncates a ninth percentile line, and a
  `percentile_labels` shorter than `percentiles` silently drops the
  tail; a slot whose side scale is `None` (every visible value `NaN`)
  paints nothing and says so nowhere; a polyline still OVERHANGS its
  plot by up to half a bucket and is clipped rather than shortened, so
  the leading segment ends at the plot's edge mid-slope; and the
  session resolution floor derives from the VISIBLE window, so monthly
  buckets could in principle relabel on a zoom (unreachable from the
  frequencies §4 offers).

**Budget (§8.4).** `cargo bench -p geode-chart`, 500,000 points into
1,600 columns with a `NaN` every 5,000, on the `bench` profile:
decimation alone **0.847 ms**, decimation plus the `PathBuilder`
rebuild **1.513 ms** — §8.4's 2 ms holds, with 0.49 ms of headroom and
criterion's own upper bound for that row at 1.58 ms. That is the price
of a cache MISS, once per visible slot, on the frame after a
pan/zoom/resize/delivery; a hit pays neither half. `docs/perf.md` holds
the conditions and says plainly that the cache, not the margin, is what
makes it safe.

**Harness.** Eighteen `chart:` entries (§11.2's two plus sixteen),
every one verified `caught` by the test it names. Four of them mutate a
CACHE KEY rather than a calculation, because a key missing a term does
not fail — it serves last frame's path with every assertion about this
frame still green; the eighteenth mutates the density bound, which
likewise changes no painted pixel, only how many of them there are.

**What is pixel-unverified: everything painted.** The implementation
sandbox has no window, so the harness can see that nothing panics and
that the caches do their job, and nothing else — a painted `text_color`
is unobservable in a `TestAppContext`, the same limit the market-data
panel records. `cargo run -p geode-chart --example chart` is the
display check (two panes, `s3` on the lower left, density on, three
percentiles per slot, a `NaN` gap every 97 buckets, 2,000 one-minute
buckets over five 400-bar days; `h`/`l` pan, `=`/`-` zoom, `0` reset —
the example's own keys, not Part 4's). What to look at, from the
reviews: that the y-axis labels land inside their 44 px column and the
x labels sit centred in the 18 px strip without colliding at the plot's
edges; that the grid's dashes read as a grid rather than noise; that a
percentile tag at `plot.right() - 2` does not sit on the right axis's
own tick labels; that a density bar at 45% fill is distinguishable from
the polyline of the same colour, and that two slots sharing a strip do
not simply obscure one another (they overdraw by design); that the
crosshair spans both panes, stops at the x axis, and that the tooltip
flips near an edge; that three theme chart colours are actually
separable on the default theme; and — the content mask, which no test
can see — that at the zoom floor (`=` held down) a polyline stops
cleanly at the plot's left and right edges instead of running over the
y-axis column, and that a percentile tag at the very top of the lower
pane sits below its line rather than half-cut in `PANE_GAP`.

## 9. The module (`geode-timeseries`)

### 9.1 Roster and contract

Kind and context `timeseries`; `contexts()` is the default. The
factory registers `timeseries::*` actions, ships its keymap fragment,
and builds a `TimeseriesTile` per tile from restored state.
`TileContent`: `key_context` (`timeseries`, `mode = normal | insert`),
`handle_action`, `command`, `completions`, `deliver` with arms for
`Series` and `SeriesFetched` and the existing variants, `set_visible`,
`session_state`, `holds_focus` off its two inputs (the expression
field and the picker's field).

### 9.2 Model (`core::model`, pure)

```rust
pub struct Model {
    slots: Vec<Slot>,             // slot numbers are stable for the tile's life
    cursor: Option<usize>,        // index into slots
    range: Range,                 // Relative("1y") | Absolute(from, to)
    frequency: Frequency,
    axis_mode: AxisMode,          // Session | Continuous
    split: f32,                   // upper pane's share of the height when a lower pane exists; default 0.7
    density: Option<u32>,         // bins; None = off; default Some(40)
    percentiles: Vec<f64>,        // default [0.05, 0.5, 0.95]; empty = off
    view: View,
}
pub struct Slot { number: u8, kind: SlotKind, colour: Colour, axis: Axis, visible: bool, state: SlotState }
pub enum Axis { Left, Right, BottomLeft, BottomRight }   // ruling 12; `pane()` answers Upper | Lower, `side()` Left | Right
pub enum SlotState { Idle, Fetching, Failed(String) }
```

Every verb is a method returning a `Changed` bitset (`Query`, `Fetch`,
`Chrome`, `Session`) so the tile knows what to do next and the tests
can assert it. The key table and the command line are two front ends
over these methods.

### 9.3 Header strip

In order: the stack marker when the tile is a member (tile-stacks
design §5), the title `Timeseries`, `1y · 1d` (or `2025-01-01 →
2026-09-19 · 1h`), then one chip per slot: swatch, label, and the
axis as `L`, `R`, `BL` or `BR`.
The label is the identity (`SPX.close`, with `@source` only when the
source is not the default), or an expression's text, or its `s3`
handle when the text is over `LABEL_MAX` = 24 characters. Hidden slots are
dimmed and struck. Chip tones through `chip_paint`: `Neutral` on the
cursor's chip, `Warning` while `Fetching`, `Danger` on `Failed` with
the reason as the tooltip. The footer paints the context's live keys
through `tips::Chords`.

### 9.4 Keys (normal mode, context `timeseries`)

| key | verb |
|---|---|
| `a` | open the picker (§9.6) |
| `x` | open the expression field (§9.7) |
| `tab` / `shift+tab` | move the chip cursor (a count jumps that many; **as built:** needs the shell's `GeodeShell` reclaim — §9.13) |
| `v` | show/hide the cursor's slot |
| `y` / `Y` | cycle the cursor's slot's axis forward / back through `left → right → bottomleft → bottomright` |
| `[` / `]` | shrink / grow the upper pane by `SPLIT_STEP` = 0.05 while a lower pane exists |
| `c` | cycle the cursor's slot through the palette |
| `b` | cycle the cursor's slot's bucket rule |
| `d` | remove the cursor's slot (no confirm; dependants removed with a notice) |
| `e` | reopen the cursor's expression for editing |
| `L` | open the series popup (§9.5) |
| `r` | open the range popup (§9.8) |
| `f` / `F` | step frequency up / down |
| `D` | toggle density |
| `p` | toggle percentiles |
| `h` / `l` | pan the view by `PAN_FRACTION` = 0.1 of the window |
| `=` / `-` | zoom about the view centre by `ZOOM_FACTOR` = 1.25 (**as built:** `+` is unspellable in a keymap — §9.13) |
| `0` | reset the view to the range |
| `g` / `G` | jump the view to the start / end |

The context opts into counts, so digits are count prefixes (which is
why there is no digit-to-slot jump) and counts apply to `h`, `l`,
`=`, `-`, `f`, `F`, `tab`, `y`, `Y`, `[`, `]`. Every key is a fragment binding whose
predicate is `timeseries && mode == normal`.

### 9.5 The series popup

`L` opens a popup over the plot painted with the market-data popup's
geometry (`popover_style`, `PopupMenu` row geometry on the rem scale).
One row per slot: swatch, label, `source · rule`, axis letter, state.
`j`/`k` move the same cursor the chips show; `v y Y c b d e` apply as
in normal mode; `enter` and `escape` close. `close_popup_with_window`
is the one closer; any dispatched action outside the popup's verbs
closes it first; a row click moves the cursor. A chip click in the
header moves the cursor without opening the popup.

### 9.6 The picker

`a` opens a `ChoiceList` (the 2026-09-19 choice core) over every
catalogued identity across every fetch source, each row `identity` in
the first column and `source` in the second, ranked by the typed text;
`enter` picks the highlighted row and adds `(identity, source)`. A
typed text that matches no row and is non-empty offers a final row
`add "<text>"…` which, picked, opens a second `ChoiceList` of the
fetch sources with the default source highlighted; `enter` adds the
pair. The picker's field is tile-owned; opening it is insert mode,
closing it blurs then drops. Rows already loaded in this tile are
marked and still pickable (a second slot of the same pair with a
different rule is legitimate).

### 9.7 The expression field

`x` opens a one-line tile-owned `Input` on the strip below the header,
prefilled with the cursor's expression on `e`. `enter` parses (§7);
an error paints inline under the field and the field stays open, as
a cell parse error does; success adds or replaces the slot and closes.
`escape` closes, discarding. Blur-then-drop on close.

### 9.8 The range popup

`r` opens a popup with two segmented `DateField`s (the market-data
panel's, landing on the day segment of `from`) and a row of presets
`1w 1m 3m 6m 1y 2y 5y`; `enter` commits, and a preset click, or `1`
to `7` while the popup holds the keyboard, commits at once. A relative range is stored relative and
resolved at each query so a restored `1y` tile is a year to today.

### 9.9 Command line

`:add <identity>[@source]`, `:expr <text>`, `:remove s<n>`,
`:rule s<n> last|first|mean|min|max`, `:colour s<n> <name>`,
`:axis session|time`, `:freq 1m|5m|15m|1h|1d|1w`,
`:range 1y | <from> <to>`, `:pct 5 50 95 | off`, `:density 40 | off`,
`:yaxis s<n> left|right|bottomleft|bottomright`, `:split 0.7`,
`:clear`. Completions per position are the bare word, per the
`TileContent::completions` contract. `:add` with no suffix and no
default source refuses with "name a source or set a default".

### 9.10 Data flow

- `:add` or a pick: the slot is `Fetching`, `DataHandle::fetch` for the
  tile's resolved range, `Changed::Chrome`.
- `Delivery::SeriesFetched` for a held pair: on `Ok` the slot is
  `Idle` and a `Request::Series` is sent; on `Err` the slot is
  `Failed(reason)` and nothing is sent.
- Any `Changed::Query`: `Request::Series`, staged under the barrier.
- A range change: one `fetch` per source slot for the new range (the
  data tier subtracts coverage) and a `Request::Series` for the
  cached part at once, so the chart repaints what it has while the
  gaps arrive.
- `set_visible(false)`: cancel in flight; `set_visible(true)`: refetch
  and requery.
- A restored tile: every source slot `Fetching` on first
  `set_visible(true)`.

### 9.11 Session

`TileState` in `session.toml`, opaque to the shell: slots (kind,
colour, axis, visible, rule; expressions by text), range, frequency,
axis mode, split, density, percentiles. Not the view: a restored tile shows
its whole range. Not slot state. Round-trip tested.

### 9.12 Config and settings

`[timeseries] default_source = "<source name>"` in the user layer,
with a settings row (`Choice` over the fetch sources, `i`/`enter` on
the choice core) and a diagnostic when it names no fetch source. Read
by the tile through a `geode_shell::series::SeriesSettings` gpui
global, written only by the shell at startup, on the settings row and
on reload, on the `UiSettings` pattern. It is the workspace's third
global, admitted under the CLAUDE.md exception because it is an
app-wide preference a module must read live: the settings row must
reach an open tile, and neither `ConfigReloaded` (views and dimensions
only) nor the factory (create time only) can carry it.

### 9.13 As built (Part 4)

Part 4 (the `geode-timeseries` module) is built, and with it §1.1.
Where the code differs from §9 above, the code is the specification
now. The crate depends on `geode-core`, `geode-shell`,
`geode-widgets`, `geode-chart`, `geode-data` (for `DataHandle` alone,
the one door a module asks for data through), `gpui`,
`gpui-component`, `chrono` and `toml`; the `Axis` vocabulary is
imported from `geode-chart` per §8.5 rather than declared a second
time.

Built as written: the roster entry and the `TileContent` contract
(§9.1), the pure `Model` with its `Changed` bitset (§9.2), the header
strip and its chips (§9.3), the key table (§9.4, with the two
amendments below), the series popup (§9.5), the two-stage picker
(§9.6), the expression field (§9.7), the range popup (§9.8), the `:`
vocabulary and its completions (§9.9), the session round trip (§9.11)
and the `[timeseries] default_source` global with its settings row and
diagnostic (§9.12).

**The nine controller decisions in the plan are all built as
recorded.** One dataset per tile (`Model.dataset`, set by the first
source slot and cleared with the last; the refusal reads `this tile
plots 'series'; 'x' feeds 'other'`); the fetch-source list riding in
the same global as the default source (`SeriesSettings` in
`geode_shell::series`, config truth — a source the engine could not
start answers `Err` on fetch and the slot paints `Failed`); the stats
window
following the VIEW (`Model::view_changed` answers `CHROME | QUERY`
while percentiles or density are on, `CHROME` alone otherwise); the
500,000-point cap pre-checked in the model with the service's own
`cap_message`, so `f`/`F`/`:freq`/`:range` refuse in place; no
coverage hull anywhere in the tile; a restored expression that no
longer resolves dropped with a named notice (`core::session`, one
notice per drop); `:colour` taking a `[colours]` name or a palette
index `1`–`5` (`TimeseriesTile::colour_named`, which refuses with
`no colour named 'x' — 1..5 or a [colours] entry`); the picker's
`add "<text>"…` row going straight to the pair when the text already
names one; and the view re-clamped on every delivery and reset on a
range change.

- **The tile paints no coverage hull, which closes §6.6's open
  question.** §6.6 left "the coverage statement ignores as-of" for
  this part to decide, on the grounds that what a tile should SHOW
  under a historical as-of was a display question no section
  answered. Controller decision 5 answers it: the only provenance any
  surface reads is `SlotProvenance.health`, in the series popup's
  state column (`popup::state_text`). Nothing paints `loaded` or
  `latest_received_at`, so the coverage read's missing time bound has
  no display consequence and stays a data-tier note.
- **`Model::add_expr` returns `Result<(u8, Changed), String>`
  (amends §9.2).** The plan's `(u8, Changed)` obliged every caller to
  `expect` on `take_number()`, which panics at slot 255 — a tile that
  has added and removed 255 slots in one session. Every caller now
  propagates it, as `add_source` already did.
- **`request::window` answers `Option<(DateTime, DateTime)>`.** It
  read `buckets[lo]` and `buckets[hi - 1]` unguarded; a tile with a
  result but no buckets (an empty first answer) panicked. `None`
  means "there is no visible span to speak of", and `request::params`
  falls back to the whole resolved RANGE as the stats window rather
  than refusing to ask at all.
- **`Changed::FETCH` means "fetch every source slot in the `Fetching`
  state", and the tile keeps an `in_flight` pair set beside it
  (amends §9.10).** §9.10 reads as one fetch per newly added slot;
  taken literally, `fetch_pending` after `:add VIX` would re-ask for
  SPX's whole span too. The state is what selects: `add_source` marks
  only the new slot `Fetching`, `Model::set_range` and
  `mark_all_fetching` mark every one. `in_flight` (a set of
  `(source, identity)`) suppresses a second ask for a pair already
  out, and `in_flight_range` is the range those asks were made under
  — a range change is a different span, so the set is dropped and the
  new span asked for. `prune_in_flight` drops an entry whenever slots
  LEAVE (`remove`, which takes an operand's dependants with it, and
  `:clear`), because an answer for a dropped pair never clears its
  own entry and a stale one is indistinguishable from a live fetch:
  the same pair, re-added, would be skipped for the tile's life.
- **One fetch per PAIR, not per slot (amends §9.10).** Two slots over
  the same `identity@source` — the same series at two bucket rules —
  are one span, and the second request would be answered entirely out
  of coverage. `fetch_pending` de-duplicates before it sends.
- **`set_visible(true)` always refetches and requeries only when the
  tile already has a result (amends §9.10's "refetch and requery").**
  A never-fetched tile has nothing to query for: the first paint
  always arrives through a `SeriesFetched Ok`, so querying on show
  would spend a round trip to paint an empty chart a beat sooner.
  Both halves matter for the restored tile of §9.10's last bullet,
  which is exactly the never-fetched case.
- **An as-of change REFETCHES as well as requeries, gated on a real
  as-of move.** `AsOf::At(t)` resolves the range to `(t − preset, t)`,
  and live fetching never covered anything before `now − preset`, so
  a requery alone paints a truncated left edge with nothing on screen
  to say so. The frame observer's followed branch clears `in_flight`
  explicitly (`fetch_pending` drops the set only when the RANGE moved,
  and an as-of change leaves `Range` identical), marks every slot
  `Fetching` and fetches, then requeries. The gate is
  `acted.is_some_and(|a| differs_on_followed(a, now))`, not
  `follows_changed`: that answers true while `acted` is `None` — a
  tile that has never asked — so on a freshly shown tile with its
  first fetch still out, ANY frame notify (a scope keystroke) asked
  for every pair's span a second time.
- **`SeriesFetched Ok(0)` requeries like any other `Ok`.** Zero means
  "the span is covered", not "there is nothing there": the data tier
  subtracts coverage before it queues a span. Gated on `n > 0`, a
  tile whose data is already local would never ask for it and would
  paint the empty hint for ever.
- **`Model::set_full` keeps a whole-range view whole (refines
  controller decision 9).** A view that was showing the entire prior
  range — a tile never zoomed, or one whose coverage just widened —
  follows the new full range; a view the trader panned or zoomed is
  only re-clamped into the new bounds. The reset on a range CHANGE is
  the tile's, through `apply_changed`'s `reset_view` flag, so the two
  answers are separable: redelivery tails, a range change resets.
- **The chart model is rebuilt only when a field `chart::build`
  actually reads has moved (`ChartKey` in `tile.rs`), and the version
  is bumped only on a real rebuild (refines §8.5's contract).** §8.5
  hands Part 4 "bump `ChartModel.version` on ANY model change", which
  taken literally makes a cursor move copy every slot's values at the
  500,000-point cap and flush every cached path for a model identical
  to the one it replaced. `rebuild_chrome` therefore always
  re-prepares the header, the title and an open series list, and
  compares a `ChartKey` — the result's identity, each slot's
  number/colour/axis/visibility/text, the frequency, the axis mode,
  the split, whether density is on, the default source, the 28-value
  theme signature and the named-colours `Arc` address — before
  building anything. §8.5's contract is preserved exactly where it
  bites: a rebuild bumps, a skip does not. The result's identity is a
  monotonic `result_seq`, NOT the `Arc`'s address, which is ABA-prone
  — the allocator hands the same block back when one result replaces
  another between two frames, and the chart would then paint the old
  points under the new model's key. One input is deliberately NOT in
  the key: `offset_secs`, read from the local clock, so a DST
  transition with a tile open is picked up at the next real rebuild
  rather than at the transition — accepted, and no worse than the
  chrome-rebuild-only refresh it replaced.
- **An unfilled chip drops its text colour with its fill, to
  `muted_foreground` (refines §9.3).** `Tone::Neutral`'s 3:1
  guarantee is measured over its OWN fill, so `secondary_foreground`
  painted straight onto the tile background is covered by no sweep at
  all. Three pairings join `geode_shell::shell::control::shipped()`:
  the `Tone::Warning` and `Tone::Danger` slot chips on a tile
  background (a different ground from the title bar's as-of chip) and
  the range popup's `Tone::Neutral` preset chips on a popover. The
  unfilled chip needs no entry — `muted_foreground` on `background`
  is the bare pairing `shipped()` already carries.
- **A popup that holds the KEYBOARD keeps only its own four verbs
  (`popup_survives` in `dispatch`).** §9.5's "any dispatched action
  outside the popup's verbs closes it first" is stage-aware: the
  series list, which holds no field, stays open through the verbs
  that change what it shows (`v y Y c b d e`, `j`/`k`, and the doors
  that replace it), while an insert popup keeps only `commit`,
  `cancel`, `insert_up` and `insert_down`. The palette can dispatch
  any action over an open field (`ctrl+k` is a chord), and a verb
  that ran with the field still installed would leave `key_context`
  reporting `insert` with nothing focused — a tile deaf to every bare
  key until `escape`. A consequence worth stating: `L` or `r` over
  another popup closes it and opens theirs, and a second `r` reopens
  the range popup on a fresh seed rather than toggling it shut
  (`escape` is its close; §9.8 gives `r` no toggle).
- **The picker splits an option with `rsplit_once('@')` and a typed
  pair with `split_once` (refines §9.6).** Its own options are built
  here as `{identity}@{source}`, so the source is the LAST `@` piece
  and a catalogued identity carrying an `@` of its own — a REST path
  — still splits correctly. A text a trader TYPED is a pair only when
  it has exactly one `@` with both sides non-empty and the right half
  names a configured fetch source; anything else is an identity
  awaiting a source, including a text whose right half names no
  source, which a REST path may legitimately look like. The add row
  is offered on the TRIMMED text, so whitespace alone offers nothing
  to commit, and a source stage with no fetch source configured
  closes with `no fetch source is configured` rather than opening a
  dead end.
- **A slot's popup state reads `failed: {reason}`, from the slot's
  own fetch or the delivered load lane (refines §9.5).** A load
  lane's `Health::Failed { reason }` was painted as a bare `failed`,
  dropping the one sentence that says what went wrong; it now carries
  the reason, exactly as a slot's own `SlotState::Failed` does.
- **In the range popup a bare `1`..`7` is a preset only while
  `!edited && !typing()` (refines §9.8).** `!typing()` is the obvious
  half — a second digit belongs to the segment being typed. `!edited`
  is what makes both halves of the popup reachable: every preset
  digit but `8`, `9` and `0` is also a legal first digit of a year,
  so a popup that read `1` as a preset after the trader had moved
  onto the year segment could never be used to type `1990`. The rule
  a trader learns is "a digit is a preset until you start editing a
  date, and the date's from then on", and only a key that MOVED
  something counts as editing (`edited |= moved`), so a `tab` or a
  `right` on the last segment leaves the presets live. `typing()` is
  a new reader on `geode_widgets::datefield::DateTimeField`. An
  `Absolute` range seeds the popup from the dates it STORES and only
  a `Relative` one resolves against now and the as-of, so reopening
  `r` under a historical as-of is lossless. The popup's `tab` is
  reclaimed from `Root`'s focus cycling by `geode_timeseries::init`
  over its own `RANGE_CONTEXT` (`GeodeTimeseriesRange`), the same
  door and mechanism as `geode_blotter::init`.
- **`+` cannot be spelled in this keymap, so `=` and `-` are the zoom
  keys (amends §9.4, whose row is corrected above).** `parse_keystroke`
  splits a binding on `+`, so a literal `"+"` is an "empty segment"
  error — found on a `--demo` boot, where the module's fragment was
  dropped with a diagnostic. `shift+=` is no escape either: both
  platforms deliver shift+punctuation as the shifted character with
  the shift modifier CLEARED (`geode_shell::defaults`' module doc,
  verified against the pinned platform sources), so that keystroke
  arrives as `+`, `shift: false`, and would match nothing. What is
  lost is one convenience spelling. `geode-app` now has a test that
  builds the WHOLE production keymap — every builtin layer, the demo
  layer, the full registry and every roster factory's fragment — and
  asserts no diagnostic, so the next unspellable key fails in CI
  rather than on a boot.
- **The shell root carries an unconditional `GeodeShell` key context,
  and `tab`/`shift-tab` are reclaimed on it (amends §9.4).**
  gpui-component's `Root` binds both keys window-wide to its own
  focus cycling in the `"Root"` context, and gpui dispatches a matched
  BINDING before any `on_key_down` listener — so §9.4's `tab` chip
  cursor was dead in the real app while every test that drove it
  through the keymap passed. `ShellView::render` now carries
  `"GeodeShell"` always (`"GeodeShell GeodeModalOpen"` while a modal
  is open, so the existing conditional context is unchanged), and
  `dialog::init_reclaimed_keybindings` binds `tab`/`shift-tab` to
  `NoAction` there: the shell root sits BELOW `Root` on the dispatch
  stack, so the deeper match wins and the keystroke falls through to
  `handle_key_down`. The dialogs', command line's and palette's older
  per-surface reclaims are redundant with it and are kept, each
  naming the surface it belongs to. What is given up is `Root`'s
  focus cycling inside the shell — an affordance Geode's keyboard
  model does not use, since focus is moved by the shell's own verbs.
- **`SeriesSettings` carries the fetch-source list as well as the
  default source, and `ShellView` mirrors the names (refines
  §9.12).** One derivation over `&Config` feeds the global, the
  settings row's value list and the diagnostic, so the three cannot
  disagree. The row's first value is `(none)`, which PERSISTS as the
  absent key rather than an empty string — and creates no empty
  `[timeseries]` table on the way — so stepping back to it leaves no
  drift. The diagnostic is a WARNING that names what IS configured
  (`'x' names no fetch source (have: demo_kdb, demo_rest)`), never an
  error: a stale default costs a trader one explicit `@source`, and
  rejecting a config reload over it would be out of proportion. It is
  seeded identically at startup (`ShellView::new`) and on reload
  (`apply_reload`), like `modules_default_diagnostic`. The mirror,
  `ShellView.fetch_sources`, exists because `settings_view::rows_for`
  reads `&ShellView` alone and has no `App` to ask the global from;
  the three writers take it from the same value, so it cannot drift.
  Folding it away by giving `rows_for` a context is a recorded
  cleanup, not a defect.

**What is pixel-unverified: everything this part paints.** The
sandbox has no window, so §1.2 item 1's walk is the display check —
the header strip's chips (swatch, label, axis letter, the dimmed and
struck hidden slot, the three tones), the series popup and the
picker's two stages, the expression strip and its inline error, the
range popup's two segmented fields and its preset chips (including
whether `RANGE_HINT` and the presets fit the 240 px popup), the
footer's live chords, and the chart itself inside a real tile rather
than the example window. §1.2 items 1 and 3 are built and their
display checks are pending; item 2 was answered in Part 3.

## 10. Demo

`--demo` registers `demo_kdb` and `demo_rest` (§5.6), declares the
`[series]` dataset, sets the default source, and the demo config's
default layout gains no timeseries tile (a trader adds one). The
demo's `datasets.toml` change is additive, so no database deletion is
needed.

## 11. Tests, harness and benchmarks

### 11.1 Tests

Weighted data ≫ core ≫ tile.

- **Data tier**, on a real DuckDB with generated rows, asserting
  values: append then refetch the same span grows nothing; a corrected
  value wins live and loses under an earlier as-of; coverage
  subtraction yields exactly the missing half-open spans (property:
  loaded ∪ missing = requested, disjoint); retention keeps the live
  row and deletes the superseded one; `history` deletes coverage with
  rows; a source without `Fetch` is `Failed` on the discovery lane at
  open; a failed fetch lands on the load lane keyed by the pair and
  clears on the next success.
- **Compiler**: SQL-text per rule, both as-of forms, the inner join
  with a gap, the zero denominator, percentiles, bins, the cap;
  end-to-end value tests for each.
- **Expression parser**: precedence, unary minus, every reference
  form, ambiguity, cycles, the "arithmetic only" message.
- **Chart core**: decimation never loses a column's min or max
  (property); session ticks strictly increase and respect `TICK_GAP`;
  `View` clamps; `Layout` reserves each side's axis and the strip only
  when asked, opens the lower pane only while a visible slot uses a
  bottom axis, honours and clamps `split`, and gives both panes the
  same x mapping; the palette sweep over every bundled theme, no
  exception list.
- **Tile**, `TestAppContext` with the market-data fixture pattern: key
  sequences against the model; a delivery staged and promoted; a
  stale tag dropped; `d` on an operand removes the dependant with a
  notice; a restored tile refetches once; `holds_focus` on both
  inputs; blur-then-drop on both closers; the chip tone sweep.

### 11.2 Harness

One `scripts/mutation-check.sh` entry per behaviour, each naming its
test: the `received_at` dedupe, the as-of `received_at` filter, the
`ts` clip, the inner join, the zero-denominator guard, the cap, the
coverage subtraction, the unchanged-row skip, the superseded-only
retention delete, the session-axis tick placement, decimation's
min-max pair, and the dependant removal (`timeseries: dependant
removal is transitive`, caught by
`removing_an_operand_removes_its_dependants_transitively`).
`--anchors-only` before every merge.

### 11.3 Benchmarks

`geode-data/benches/series.rs`: the series query at one million rows,
one and four slots, against 50 ms. `geode-chart/benches/decimate.rs`
per §8.4. Both `harness = false`; numbers into `docs/perf.md`.

## 12. Sequencing

Four parts, each its own branch, review and merge:

1. **Data tier** (§4, §5): family, storage, append, coverage,
   retention, `Fetch`, `Request::Fetch`, catalogue, health, demo
   adapter. Done when §11.1's data-tier tests pass with no UI.
2. **Series query** (§6, §7): `geode_core::series`, the parser, the
   compiler, `Request::Series`, `SeriesOutcome`, `Delivery::Series`
   and `Delivery::SeriesFetched` (every occupant gains its arms here;
   `SeriesFetched` is delivered by part 1's event but the variant
   lands with its consumer, so part 1 logs the event and part 2 routes
   it).
3. **`geode-chart`** (§8): core and element, proven in a throwaway
   example window on generated arrays before the module exists;
   the bench.
4. **Module** (§9, §10): tile, header, popup, picker, expression
   field, range popup, command line, session, fragment, settings row,
   `--demo` wiring, `docs/perf.md`.

Display checks on a real window follow each part; the implementation
sandbox cannot paint one.

## 13. Open questions

- Whether the desk's KDB holds one table per identity or a symbol
  column; the `Fetch` trait does not care, the shim does. Waits for
  the work machine.
- Whether a REST source's identities should be saved as a user-layer
  catalogue once used, so `demo_rest`-style sources gain typeahead
  over time. Not built; the catalog is the seam.
- Live tail's producer shape (§1.3).
