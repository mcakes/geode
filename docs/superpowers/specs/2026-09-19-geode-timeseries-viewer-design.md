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
   fetch, answers.
2. A series query over one million cached rows returns under 50 ms;
   decimating 500,000 points to pixel columns and rebuilding the paths
   takes under 2 ms; a frame with nothing changed allocates nothing
   in the chart element.
3. A corrected value for an existing `ts` wins live and loses under an
   as-of set before its `received_at`; an overlapping refetch does not
   grow the table; a tile restored from `session.toml` refetches once
   and paints what it painted before.
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
cliff). Then, once, the shared x-axis under the lowest pane and the
crosshair, which spans both panes at one x with the readout listing
every visible slot from either pane, through the component's
tooltip. Labels `p5 p50 p95` are painted at the right end of each
line in the slot's colour through `prepaint`'s child elements. A
slot's percentiles and bins are drawn in its own pane against its
own axis.

A frame with an unchanged key pushes cached paths and allocates
nothing; the cache invalidates on model version, view or bounds.

### 8.4 Budget

A criterion bench, `geode-chart/benches/decimate.rs`: 500,000 points
into 1,600 columns, target under 2 ms including the path rebuild.
Numbers into `docs/perf.md`.

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
| `tab` / `shift+tab` | move the chip cursor (a count jumps that many) |
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
| `=` `+` / `-` | zoom about the view centre by `ZOOM_FACTOR` = 1.25 |
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
min-max pair, the dependant removal. `--anchors-only` before every
merge.

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
