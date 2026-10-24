# Real sources

Every external system Geode talks to — the subscribed market-data bus, the
upload target, history stores, the reference database, the position system,
the pricing library — is currently served by a simulator. This guide says
where a real vendor client plugs in, what the simulator does in its place,
and which parts of each shape were guessed and must be checked against the
real feed. Runtime behavior behind each seam (queues, health lanes, recovery,
shutdown) is in [the data path](data-path.md); configuration rules are in
[configuration](configuration.md).

## Principle

A vendor client is a thin shim. It moves bytes or rows across one small
trait and nothing else:

| The shim does | Geode does, vendor-independently |
|---|---|
| Connect, authenticate, subscribe to topic patterns, report connection state | Topic recording, recovery windows, health lanes |
| Hand over each message as topic + arrival time + whole body bytes | Parsing (`DocumentKind`), validation, source-time stamping, coalescing, publication |
| Answer a history request with timestamp/value arrays | Coverage bookkeeping, bitemporal append, queries |
| Answer a keyed history request with key, timestamp and value columns | Key normalization, ask checking, coverage per key, bitemporal append, reads |
| Answer a table read with named columns | Conforming to the declaration, unchanged detection, publication |
| Send encoded bytes to an address | Encoding (`DocumentKind::write`), confirm, echo comparison |
| Bound its own calls with timeouts | Panic containment, supervision, refusal reporting |

The shim never parses a document, never touches DuckDB, and never decides
health beyond connection state. A shape that turns out wrong is then fixed
in one place, mostly without touching the shim.

Where a guessed shape is corrected differs by family. Not every shape is a
config-declared column map today:

| Shape | Where it is declared | Corrected by |
|---|---|---|
| Risk CSV headers | `datasets.toml` `source_name` per column | Config |
| Risk file naming, partition key | `sources.toml` `paths`, `batch_pattern` | Config |
| Topic patterns | `sources.toml` `topics` | Config |
| Upload addresses | `egress.toml` `documents` templates | Config |
| Source time policy | `sources.toml` `source_time` | Config |
| Dataset columns, types, keys, axes | `datasets.toml` | Config, but each document kind's `columns()` must agree (checked at source start) |
| XML element names and nesting | Rust constants in `geode-documents` (`SLICE_VALUES`, `TAGS`, the path matchers) | Code: one module per kind |
| Reference column names | Must equal the declared column names; `TableRows::conform` matches by `name`, not `source_name` | The shim or a database view |
| Series identity spelling | Adapter-defined string; expression grammar limits the characters | The shim |
| History key values and table names | Key values in `underlying_ref` spelling; `table` names the vendor table | The shim; `sources.toml` `table` |

## Seams at a glance

All adapter traits live in
[`crates/geode-data/src/adapter/mod.rs`](../../crates/geode-data/src/adapter/mod.rs).
An `Adapter` is registered by name in an `AdapterRegistry`; each capability
method returns a fresh handle per call, or `None` when the adapter lacks it.

| Capability | `Adapter` method → trait | Owning worker | Configured in | Simulator today |
|---|---|---|---|---|
| Subscribed documents | `subscription()` → `Subscription` (+ optional `Recovery`) | `SubscriptionWorker` (`ingest/subscribe.rs`), one per source | `sources.toml` (`topics`, `document`) | `ChannelAdapter` `demo_bus` fed by `geode-compose`'s `demo_bus` |
| Uploads | `egress()` → `Egress` | One egress worker per target (`egress.rs`) | `egress.toml` | `ChannelAdapter` `demo_bus` (publishes back onto the bus) |
| History series | `fetch()` → `Fetch` | `FetchWorker` (`ingest/fetch.rs`), one per source | `sources.toml` over a `series` dataset | `DemoSeries` (`demo_kdb`, `demo_rest`) |
| Reference tables | `snapshot()` → `SnapshotQuery` | `SnapshotWorker` (`ingest/snapshot.rs`), one per source | `sources.toml` (`table`, `poll_interval`) | `DemoRefDb` (`demo_refdb`) |
| Keyed history | `history()` → `HistoryQuery` | `HistoryWorker` (`ingest/history.rs`), one per source | `sources.toml` (`table`) over a `history` dataset | `DemoHanweck` (`demo_hanweck`) |
| Position commands | `positions()` → `PositionCommands` | `PositionWorker` (`positions.rs`) | `positions.toml` | `DemoPositions` (`demo_positions`) |
| Risk files | none: built-in directory discovery, `adapter = "csv_dir"` | Discovery scheduler (`source/discovery.rs`) | `sources.toml` (`paths`, readiness) | `geode-demo-data` CSV emitter |

Calculation seams are not adapters. They live in `geode-core` and are
registered separately:

| Seam | Trait | Registry | Configured in | Stand-in today |
|---|---|---|---|---|
| Line pricing | `geode_core::pricing::Pricer` ([`pricing.rs`](../../crates/geode-core/src/pricing.rs)) | `geode_data::PricerRegistry` | `app.toml` `[pricing] adapter` (default `mock`) | `geode_pricing::MockPricer` |
| Vol surface evaluation | `geode_core::vol::VolModel` ([`vol.rs`](../../crates/geode-core/src/vol.rs)) | `geode_data::VolModelRegistry` | `app.toml` `[vol] model` (default `demo`) | `geode_pricing::DemoVolModel` |
| Document wire format | `geode_core::document::DocumentKind` ([`document.rs`](../../crates/geode-core/src/document.rs)) | `geode_data::documents::DocumentRegistry`, filled from `geode_documents::builtin_kinds()` | `sources.toml` `document`, `panels.toml` | `CviKind`, `DividendKind`, `OptionChainKind` (real code, guessed tags) |

Registration is split by what a headless process needs.
[`geode_compose::adapters`](../../crates/geode-compose/src/lib.rs) builds the
`AdapterRegistry`, and `geode_compose::engine_setup` registers
`geode_documents::builtin_kinds()`; the app and the background collector
(`geode-collector`) share both.
[`crates/geode-app/src/main.rs`](../../crates/geode-app/src/main.rs) builds
the `PricerRegistry` and `VolModelRegistry`, and
[`bridge.rs`](../../crates/geode-app/src/bridge.rs) resolves the pricer,
egress targets and position service against these registries.

## Source families

### Risk files

**For:** the blotter's position-level risk (`risk_snapshot` in the demo).

**Simulator:** `geode-demo-data`'s `generate` and `emit` modules write
seeded CSVs and `.done` sentinels into `$TMPDIR/geode-demo/<rows>-42/src/`
when `--demo` starts with an empty directory. The demo layer
(`demo::layer` in [`demo.rs`](../../crates/geode-compose/src/demo.rs))
generates the `[demo]` source: `paths = ["…/*.csv"]`, `readiness = "sentinel"`,
`poll_interval = "2s"`, `batch_pattern = '^risk_\d{4}-\d{2}-\d{2}_(?P<batch>.+)$'`.

**Seam:** none to write. Directory discovery is built in (`csv_dir`, the
default adapter) and works without `--demo`. A real deployment needs only a
desk-layer `sources.toml` entry and a `datasets.toml` declaration.

**Config:** `sources.toml` (`paths`, `readiness`, `batch_pattern`,
`priority`, `poll_interval`, `pending_timeout`); `datasets.toml` columns with
`source_name` mapping file headers to canonical names, roles and grains.

**Guessed — check against the real drop:**

- The readiness contract. Discovery requires a JSON sentinel at
  `<csv filename>.done` carrying `as_of` (RFC 3339) and `columns`. This is
  Geode's own contract: if the real producer writes no such file, nothing
  loads. The only alternative, `readiness = { stable_mtime = N }`, parses but
  is not implemented (every candidate reports unusable).
- Source time comes from the sentinel's `as_of`, not the file's mtime. Check
  what instant the real producer means by it.
- File naming and partitioning: one file per date × book group, batch taken
  from the stem by `batch_pattern`. A position must not be split across
  files, or its measures double-count.
- Header names (`source_name` values such as `SC`, `Delta01`), the measure
  grains (`position`, `instrument`, `underlying`, pair), and the `_usd` twin
  columns. Grains matter most: a wrong grain produces plausible wrong totals.
- CSV dialect: the demo emitter writes no quoting or escaping.
- Key spellings: `underlying_ref`, `position_ref`, `instrument_ref` values
  are assumed to match the keys every other family uses (below).

#### Scenario files

**For:** bucketed and scenario risk — vega by tenor and moneyness, a vega
spot ladder, a spot ladder, term sensitivities and rho by curve
(`vega_buckets`, `vega_spot_ladder`, `spot_ladder`, `term_sensitivities`,
`rho_curves` in the demo), each a measures dataset with `axes`. Term
sensitivities come from five reports, one per measure: repo, ATM vol,
skew, curvature and dividend sensitivity (diva).

**Simulator:** `geode-demo-data`'s `scenario` module derives one table per
dataset from the risk batch, and one per term report for
`term_sensitivities`. `emit_scenario_directory` writes
`<dataset>_<date>_<book>.csv` (`<report>_<book>_<date>.csv` for a term
report) with `.done` sentinels into
`$TMPDIR/geode-demo/<rows>-42/scenario/`, beside `src/` so the risk glob
never picks one up. Bucket measures are unjittered shares of the row's own
risk measure, so a sum over a bucket axis reproduces the blotter's value;
the vega spot ladder and curvature are jittered. Only the first
`SCENARIO_COVERAGE` (5,000) distinct (instrument, underlying) rows get
scenarios, whatever the demo row count, so the scenario store stays bounded.
The demo layer declares one `csv_dir` source per dataset,
`demo_scenario_<dataset>`: sentinel readiness, a five-second poll, and
`batch_pattern = '^<dataset>_\d{4}-\d{2}-\d{2}_(?P<batch>.+)$'`.

`term_sensitivities` has no source of its own. Each of its five reports is
named for its measure (`repo`, `atm_vol`, `skew`, `curvature`, `diva`)
and is its own file, `<report>_<book>_<date>.csv`, carrying the identity
columns, `Tenor`, the report's measure and its `_USD` twin. Its source,
`demo_scenario_term_<report>`, lists that measure and twin in `measures`
and uses `batch_pattern = '^(?P<batch><report>_.+)_\d{4}-\d{2}-\d{2}$'`,
so its batch is `<report>_<book>`. The batches must differ between
reports: a partition is (dataset, batch, book), and a load replaces its
partitions' live rows, so two reports under one batch would delete each
other's rows on every load. The book comes before the date in the file
name because a batch is one capture and must leave the date out. Diva per
bucket is `-delta01 × bucket width in years`: higher dividends lower the
forward, so it opposes delta and grows with the bucket's time.

**Seam:** the same as risk files: none to write. A real deployment needs a
desk-layer `sources.toml` entry per report and a `datasets.toml`
declaration with `axes`. Where several reports feed one dataset, each
report's source lists the `measures` its files carry and each source's
`batch_pattern` yields a batch no other report's does. Each demo term
report's source lists its one measure and that measure's twin. Splitting
a report into several, or changing a `batch_pattern`, leaves the old
batch's partitions live beside the new ones: nothing retires them, and
their measures add in with `Ok` health. Delete the store after such a
change (see [several reports](configuration.md#validation-boundaries)).

**Lag:** a position move rewrites the risk files only; scenario rows keep
the old LHU until the next scenario run, and the scenario data's source
time says how old the run is. The demo never re-drops its scenario files,
so after a demo move the scenario datasets keep the old LHU until the demo
directory is deleted (see
[the demo position system](data-path.md#the-demo-position-system)).

**Guessed — check against the real drop:**

- Header spellings for axes and measures (`SCENARIO_SOURCE_NAMES`, each
  measure followed by a `_USD` twin) and whether axis values print as the
  demo writes them (`0`, `-5`, no trailing zeros).
- Bucket vocabularies: the tenor list (`1M` … `5Y`), the moneyness and
  spot-shift grids, the curve currencies and types. A tenor label outside
  `<n>D|W|M|Y` sorts after the parsable ones under `order = "tenor"`.
- Whether a real run covers the whole book or a subset, and whether every
  bucket is present for every row. An absent row reads as no value, not
  zero.
- The term reports' file names, headers and bucketing, and diva's unit
  (per 1% dividend change). Each real report is assumed to carry one
  measure; a report carrying several would list them all in its source's
  `measures`.
- Whether spot shifts are joint across underlyings or per underlying. The
  demo ladders each (instrument, underlying) row on its own underlying; a
  joint shift changes what a sum across underlyings at one shift means.
- Any NULL axis value refuses the whole file, so a producer that leaves an
  axis blank for a total row must be asked to drop that row.

### Subscribed market-data documents

**For:** CVI parameters (`cvi_params`), dividend schedules
(`dividend_schedule`) and option chains (`option_chain`), each a whole XML
document replaced per key. The market-data panels (`cvi`, `dividend`) and the
vol slice viewer read them.

**Simulator:** a `ChannelAdapter` named `demo_bus`
([`channel.rs`](../../crates/geode-data/src/adapter/channel.rs)), fed by the
producers in [`demo_bus.rs`](../../crates/geode-compose/src/demo_bus.rs) using
`geode-demo-data`'s `CviGenerator`, `DividendGenerator` and `ChainGenerator`.
Producers serialize through the same `DocumentKind::write` the real path
would parse, and publish on `marketdata/{cvi,dividend,chain}/<key>/NOTIFY`.
The demo layer generates three sources over it: `[cvi]`, `[dividend]`,
`[opra_sim]`, each `topics = ["marketdata/<kind>/*/NOTIFY"]`,
`coalesce = "500ms"`, `source_time = "receive"`. Enabled only by `--demo`.

**Seam:** implement `Subscription` and return it from
`Adapter::subscription()`:

- `subscribe(topics, sink, health)` starts delivery of every topic matching
  any pattern into `MessageSink::push` as a `Message { topic, received,
  bytes, recovered: false }`, and reports `ConnectionState` through
  `health`, including the current state as part of subscribing. `push` never
  blocks; a refusal is counted and surfaced as `<source>:queue` health.
- `received` is the arrival time in UTC, stamped by the shim. It is the
  publish's source time under `source_time = "receive"`.
- `bytes` is the whole document body, unparsed.
- `unsubscribe()` is idempotent and must drop retained sinks.
- Health callbacks return promptly, never reenter the adapter, never panic.
  `Connected` → `Ok`, `Reconnecting` → `Pending`, `Lost` → `Failed`, on the
  discovery lane.

The document key comes from the parsed body (`marketData/underlying`, plus
`optionChain/expiry` for chains), never from the topic.

**Config:** `sources.toml` (`adapter`, `dataset`, `document`, `topics`,
`coalesce`, `source_time`); `datasets.toml` document declarations (the demo
copies are in
[`examples/demo-config/datasets.toml`](../../examples/demo-config/datasets.toml));
`panels.toml` (builtin panels in
[`builtin_panels.toml`](../../crates/geode-marketdata/src/core/builtin_panels.toml)).

**Guessed — check against the real feed and XSD:**

- Transport: one whole-document XML message per key per update, on a
  Solace-style topic. Partial or delta messages are not supported.
- Topic hierarchy `marketdata/<kind>/<key>/NOTIFY`. Correctable in
  `topics`. Geode's pattern grammar is whole-level `*` and final `>`; patterns
  go to the transport verbatim, but Geode's own `topic_matches` decides which
  recorded topics recovery asks for, so a vendor wildcard Geode reads as
  literal (a prefix like `SPX*`) breaks recovery selection.
- Every XML element name and the nesting (`marketData` root, `underlying`,
  `cviParams/anchorDate`, `spotRef`, `nodes/node`, `slices/slice` with
  `term`, `forward`, `atm`, `skew` and one `param` per node; `dividends` with
  `currency`, `scheduleDate`, `dividend/{exDate, announcedDate, payDate,
  amount, status}`; `optionChain` with `expiry`, `forward`, `spotRef`,
  `quoteTime`, `quote/{strike, bidVol, askVol, midVol, bid, ask}`). The
  module docs and the [geode-documents README](../../crates/geode-documents/README.md)
  mark them unverified. Namespaces are ignored (local names only); unknown
  elements are skipped and logged once per path.
- Key spelling: CVI tests use `SPX.Z`, the demo uses `SPX`. Whatever the
  feed uses must equal risk `underlying_ref` and reference-table keys, or
  blotter → panel launches and reference lookups miss.
- CVI semantics: node values as a moneyness ladder, `param` per node, one
  `forward`/`atm`/`skew` per slice, `anchorDate` as a date. `DemoVolModel`'s
  reading of them (`node/100` knots, spline, flat extrapolation) is invented.
- Dividend semantics: `announcedDate` and `payDate` always present (inbound
  values are estimates); status vocabulary `estimated`, `declared`, `paid`,
  `cancelled` (`dividend::STATUSES`, mirrored by the panel's `choices`); no
  row id on the wire — `mint_ids` derives `dividend_id` from ex date and
  same-day order, so a pure same-day reorder upstream swaps ids undetectably.
- Option chain as a subscribed XML document per (underlying, expiry), mid vol
  always present, one-sided quotes allowed, `quoteTime` RFC 3339. The desk's
  idea list names KDB/Hanweck, OPRA and Bloomberg as vol sources; whether any
  of them delivers this shape over a bus is unknown.
- Source time: `receive` everywhere. `document:<field>` needs a date or utf8
  attribute; CVI's `anchor_date` is a date, which stamps midnight UTC.
- `coalesce = "500ms"` against the real update rate.

### Recovery on subscribe

**For:** filling documents published while Geode was closed or
disconnected, by asking the transport for the latest document per topic.

**Simulator:** `ChannelAdapter` answers from the last message it dispatched
on each topic in this process. The demo bus lives in the app, so it never
holds anything from before launch.

**Seam:** return `Some(Box<dyn Recovery>)` from
`Subscription::recovery()`. `Recovery::recover(topics, timeout)` receives
concrete NOTIFY topics (never patterns), must return promptly (`Ok` means
requested), and its replies go into the same sink under the NOTIFY topic
with `recovered: true`. A transport that cannot recover returns `None`;
its keys stay stale until their next update.

**Config:** `recover_timeout` (default `10s`), `recover_max_age` (default
`7d`) on subscribed sources. Known topics persist in the store's
`subscription_topics` table.

**Guessed — stated by the desk, not yet exercised:** the GET pairs each
NOTIFY topic as `<base>/GET` beside `<base>/NOTIFY`; the reply carries the
NOTIFY payload format; the timeout travels as a request parameter; a GET
cannot carry a wildcard. Mapping NOTIFY → GET topics is the shim's job.

### Uploads

**For:** sending an edited CVI or dividend document to the system of record
(Sophis in the desk's terms) from a market-data panel, with a y/n confirm and
an echo check.

**Simulator:** the `demo_bus` `ChannelAdapter`'s `Egress::upload`, which
publishes the bytes onto the bus at the address. The demo `egress.toml`
target `[sophis]` addresses `marketdata/cvi/{key}/NOTIFY` and
`marketdata/dividend/{key}/NOTIFY`, so the upload returns through the
subscription as the echo. Option chains have no upload target.

**Seam:** implement `Egress::upload(target, bytes)` and return it from
`Adapter::egress()`. `target` is the expanded address; `bytes` are already
encoded by `DocumentKind::write`. `Ok` means whatever the adapter defines;
the worker has no timeout, so the shim bounds its own call. One worker per
target runs one upload at a time.

**Config:** `egress.toml` (`adapter`, `documents` → address template; `{key}`
is replaced by key parts joined with `/`, unescaped). Restart-required.

**Guessed:**

- The upload transport and address scheme. The demo publishes on the same
  NOTIFY topic it subscribes to; the real target may be a different topic,
  queue or API.
- The upload body is the same XML shape the feed delivers inbound.
- The echo: the panel confirms a sent draft when a later subscribed
  generation equals it (compared as a multiset). This assumes the system of
  record republishes the uploaded document on the subscribed topic,
  unchanged. Normalization (rounding, reordering, recomputed fields) shows as
  `echo differs`; no republish leaves the draft `Sent`.
- Last writer wins; there is no version or lock on the wire.

### History series

**For:** the timeseries tile (`SPX.close@demo_kdb`), fetched on demand and
cached in a `series` dataset.

**Simulator:** `DemoSeries` in
[`demo_series.rs`](../../crates/geode-compose/src/demo_series.rs), registered
twice: `demo_kdb` (enumerates 24 identities) and `demo_rest` (no catalogue,
manual entry). Deterministic one-minute bars, weekday sessions 14:30–21:00
UTC, no holidays or DST. Enabled by `--demo`.

**Seam:** implement `Fetch` and return it from `Adapter::fetch()`:

- `fetch(&FetchRequest { identity, from, to })` returns `SeriesRows { ts,
  value }` for the half-open span `from <= ts < to`, equal lengths, strictly
  ascending UTC timestamps. Repeated timestamps are refused; non-finite
  values are dropped with a warning.
- `catalogue()` returns the identities for typeahead, or `None` when the
  source cannot enumerate. Called at open and on demand.
- Both are blocking and need the shim's own timeouts: the worker has no
  deadline or cancellation and drains its queue at shutdown.

**Config:** `sources.toml` source over a `series` dataset (no `document`,
no `topics`); `datasets.toml` `family = "series"` with `retention`;
`app.toml` `[timeseries] default_source`.

**Guessed:**

- Identity spelling `UNDERLYING.field` (`SPX.close`, `SPX.vol_1m`, `VIX`).
  The expression grammar accepts identities of ASCII letters, digits, `_` and
  `.` starting with a letter or `_`, and source names of letters, digits, `_`
  and `-`. A vendor symbol with spaces, `/` or a leading digit (`SPX Index`)
  must be mapped by the shim.
- What a bar's timestamp means (open or close of the interval) and the bar
  width; the tile's frequency is display-side bucketing.
- Range semantics: the data tier asks only for uncovered gaps and assumes
  a re-fetch of the same span returns the same values.
- That one source answers one value per timestamp (no bid/ask/OHLC
  columns).

### Vol surface history (Hanweck)

**For:** constant-maturity implied-vol surface points, realized vol,
variance-swap strikes and the skew-stickiness ratio per underlying, asked
per key on demand and cached in `history` datasets (`vol_surface`,
`realized_vol`, `varswap`, `ssr` in the demo).
The vol board request reads them (see
[board requests](data-path.md#board-requests)), and the vol watchlist tile
paints the board (see [Vol watchlist](features.md#vol-watchlist)).

**Simulator:** `DemoHanweck` in
[`demo_hanweck.rs`](../../crates/geode-compose/src/demo_hanweck.rs)
(`demo_hanweck`), behind the demo layer's `hanweck_surface`,
`hanweck_realized`, `hanweck_varswap` and `hanweck_ssr` sources. Every value
is a pure function of seed, key and day: one close per weekday at 21:00 UTC
from 2024-06-03, plus five-minute intraday points from 14:30 UTC up to now
on the current business day only (answered by `Latest`, `At` and `Between`
alike, so a cached span answers as the source would), no holidays, and
nothing after the adapter's clock. Ten underlyings, tenors 1W to 5Y (`1W 1M
2M 3M 6M 1Y 18M 2Y 3Y 5Y`), call deltas 0.05 to 0.95, realized windows 5D,
20D and 60D, and a precomputed skew-stickiness ratio per tenor. Enabled by
`--demo`.

**Seam:** implement `HistoryQuery` and return it from `Adapter::history()`:

- `query(&HistoryRequest { table, keys, span })` returns `HistoryRows`:
  `ts`, one column per key column and one per value column in declared
  order, equal lengths. `table` is the configured name. `keys` is a
  `KeySet`: `Exact` whole tuples or `Prefix` leading values; every row
  returned must equal an exact tuple or start with a prefix, or the whole
  answer is refused. `span` is `Latest` (each key's newest point), `At(t)`
  (the last at or before `t`, an `aj` on `(key, ts <= t)` in q terms), or
  `Between { from, to, step }` over `from <= ts < to`, with `step` a
  downsampling hint the source may honor (at most one point per step, the
  last in each) or ignore.
- Keys may come back in any spelling of the same value (`0.50` for `0.5`);
  the worker normalizes them before checking. A repeated `(key, ts)` is
  refused; non-finite values are dropped with a load-note warning.
- `keys(table, column)` returns a key column's distinct values for
  typeahead and the tenor set, or `None` when the source cannot enumerate.
  Asked once per key column at open.
- Both are blocking and need the shim's own timeouts: the worker has no
  deadline or cancellation and drains its queue at shutdown.

Keys are `underlying_ref` values, Geode's own ticker spelling: the shim owns
the map to the vendor's symbols, as it does for series identities.

**Config:** `sources.toml` source over a `history` dataset with `table`
(default the dataset name), at most one per dataset; `datasets.toml`
`family = "history"` with `key`, f64 values, `history` and `retention`
windows, and `order = "tenor"` on a tenor or window key column.

| Demo table | Dataset | Key | Values |
|---|---|---|---|
| `surface` | `vol_surface` | `underlying_ref`, `tenor`, `delta` | `vol`, `forward` |
| `realized` | `realized_vol` | `underlying_ref`, `window` | `vol` |
| `varswap` | `varswap` | `underlying_ref`, `tenor` | `strike` |
| `ssr` | `ssr` | `underlying_ref`, `tenor` | `ssr` |

**Guessed:**

- The KDB table names and their column names.
- Delta convention: call delta as an f64 (0.95 is the low-strike wing),
  not put deltas or `25P`/`25C` labels.
- Tenor labels (`1M`, `18M`, `1Y`) as `<n>D|W|M|Y`; a vendor spelling
  `12M` and `1Y` for the same point would be two keys.
- That realized vol and varswap strikes are precomputed upstream, not
  derived here from prices.
- That SSR (and any further desk analytic such as realized spot/vol
  covariance) arrives precomputed per underlying and tenor; the board never
  derives it from spot and vol series.
- One observation per key per timestamp, and that history once answered
  does not change: a covered span is never asked again, so a later vendor
  correction reaches the cache only through an ask the cache cannot answer
  (`Latest`, or `At` at an uncovered instant).
- That an observation is published within 30 minutes of its timestamp
  (`HISTORY_SETTLE`): coverage from an answer ends that long before the
  call, so anything stamped earlier but published later than that is never
  asked for again. A vendor whose closes arrive later needs a longer
  margin (an open item).
- That a full-resolution window is affordable: the board's window
  `Between` asks without a `step`, so coverage can record it and later
  refreshes ask only the recent tail. A vendor holding intraday history for
  a year would answer many rows for that first ask; a daily table name
  (one close per day) would be the lever.
- That one `Latest` prefix call per dataset per refresh, with no per-call
  size cap, is acceptable. On the demo adapter one prefix call over ten
  underlyings took 785 µs against 1.32 ms for ten single-member calls
  (`docs/perf.md`); a real adapter with a size limit splits inside its
  shim.

### Reference data

**For:** the `underlyings` table (vendor tickers, currency, calendar,
exchange, multiplier), read by the pricer's payout currency and the
diagnostics Reference page through `ReferenceGlobal`.

**Simulator:** `DemoRefDb` in
[`demo_refdb.rs`](../../crates/geode-compose/src/demo_refdb.rs) (`demo_refdb`),
ten hand-written rows; every third poll renames one. The demo layer's
`[refdb]` source polls `table = "underlyings"` every `30s`.

**Seam:** implement `SnapshotQuery::query(table) -> TableRows` and return it
from `Adapter::snapshot()`. `table` is an adapter-defined name from config.
The whole table comes back as named columns (`RefColumn` per column) in any
order; `TableRows::conform` then refuses repeated or missing key columns,
type mismatches, non-finite numbers, NULL or repeated keys, and empty
results, keeping live rows. Blocking, no deadline: the shim bounds it.

**Config:** `sources.toml` (`adapter`, `dataset`, `table`, `poll_interval`,
default `5m`); `datasets.toml` `family = "reference"`, one utf8 key; at most
one snapshot source per reference dataset; `app.toml` `[pricing]
payout_currency` (defaults to `underlyings.currency`).

**Guessed:**

- The store itself (SQL database, service, file) and what `table` names.
- Column names: the adapter's names must equal the declared names
  (`underlying_ref`, `name`, `bbg_ticker`, `ric`, `currency`, `calendar`,
  `exchange`, `asset_type`, `multiplier`). `source_name` is not honored for
  reference datasets.
- `calendar` and `exchange` as ISO 10383 MIC codes; `currency` as ISO 4217
  (the mock pricer refuses codes outside its rate table).
- Whole-table polling is affordable at the real size and rate.
- Source time is the poll's start time, not anything from the source.

### Position commands

**For:** the blotter row menu's Move LHU.

**Simulator:** `DemoPositions` (`demo_positions`) in
[`demo.rs`](../../crates/geode-compose/src/demo.rs) rewrites the `LHU` field
in the demo risk CSVs and advances the sentinel, so the move arrives as a
newer file generation.

**Seam:** implement `PositionCommands::move_lhu(positions, lhu)` and return
it from `Adapter::positions()`. `Ok` means accepted; an `Err` message is
shown as the refusal reason. No timeout in the worker.

**Config:** `positions.toml` `[service] adapter`. Restart-required.

**Guessed:** the position system and its API; that positions are named by
`position_ref` values and LHUs by the blotter's `lhu` values; that a move is
all-or-nothing on validation; that the move later appears through the risk
source rather than in the reply.

### Pricing library and vol model

**For:** the line pricer (`Pricer`) and the vol slice viewer and chain
generator (`VolModel`). Both are in-process calculation leaves reached
through data-tier requests, not adapters.

**Stand-ins:** `MockPricer` (`mock`) prices plausibly but is not a model:
fixed USD rates for seven currencies, no quanto. `DemoVolModel` (`demo`)
reads CVI documents with an invented interpretation.

**Seams:**

- `Pricer`: `name`, stateful `set_overrides(&MarketOverrides)` once per
  batch, then synchronous `price(&PriceRequest { instrument, shifts,
  currency }) -> PriceResult` (local and USD arrays over the 14 `Measure`s).
  The library fetches or is handed its own market data; Geode passes only
  spot overrides.
- `VolModel`: `name`, `kind` (the document kind it reads, informational),
  `slice(&DocumentRows, &SliceRequest)`, `coordinates(&MapRequest)`.

**Config:** `app.toml` `[pricing] adapter`, `[vol] model`; both
restart-required. An unknown name is a diagnostic and every line or slice
answers with that reason.

**Guessed:** the instrument vocabulary (`Vanilla`, `Barrier`), the measure
set and its units, how the real library obtains market data, and that the
real CVI evaluator matches the `VolModel` signature. `cvi_reanchor` and
`cvi_recalc_forward` kind actions refuse until a real CVI library exists.

### External hand-offs

`geode-nemo` opens `nemo://position/{id}` and `nemo://instrument/{id}` from
the row menu. Both URL shapes are provisional until Nemo's scheme is known;
they are constants in that crate, not config.

## Adding a real client

1. **Crate.** Put each vendor client in its own crate, for example
   `crates/geode-adapter-solace`. It depends on `geode-data` (for the
   `adapter` traits) and `geode-core`, never on `geode-shell` or a feature
   module. A pricer or vol model depends on `geode-core` only. Set
   `bench = false` on its library target.
2. **Feature gating.** The vendor SDK dependency is optional behind a crate
   feature that is off by default. CI runs `cargo clippy --workspace
   --all-targets` and `cargo test --workspace` on macOS and Windows with
   default features, so any SDK reachable without a feature breaks CI.
   Without the feature the crate still builds its pure parts (topic mapping,
   symbol mapping, config parsing) and their tests. `geode-compose` owns the
   optional vendor dependency and its feature, for example
   `solace = ["dep:geode-adapter-solace", "geode-adapter-solace/sdk"]`.
   Every binary that links `geode-compose` forwards the feature, for example
   `solace = ["geode-compose/solace"]` in `geode-app`, so the app and the
   background collector register the same adapters.
3. **Registration.** Register real adapters in `geode_compose::adapters`
   under `#[cfg(feature = "…")]`, in both the demo and non-demo paths: today
   the function returns an empty registry early when there is no demo
   directory, so a real adapter goes before that return. Register the demo
   adapters only when there is a demo directory. An adapter registered only
   in the app leaves the collector unable to serve that source, and the
   test holding the app and the collector to equal configuration cannot
   catch it: `SourceSpec` equality does not check that the named adapter is
   registered. Real pricers and vol models stay in the app's `main.rs`,
   beside the mock ones in `PricerRegistry` and `VolModelRegistry`, because
   the collector prices nothing. New document kinds go into
   `geode_documents::builtin_kinds()`, which `engine_setup` registers for
   both binaries.
4. **Names.** `Adapter::name()` is what `sources.toml`, `egress.toml` and
   `positions.toml` select. Real names must differ from `demo_bus`,
   `demo_kdb`, `demo_rest`, `demo_refdb`, `demo_hanweck` and
   `demo_positions`; a duplicate
   registration replaces the earlier one with a warning.
5. **Credentials.** Read secrets from the environment. Config may name the
   variable, never hold the secret (see
   [credentials](configuration.md#credentials)).
6. **Contract checklist.** Fresh handle per capability call. `subscribe`
   and `recover` return promptly. Health callbacks are quick and never
   panic. Every blocking call (`fetch`, `catalogue`, `query`, `keys`,
   `upload`, `move_lhu`) has its own timeout. Vendor callback threads are the shim's;
   Geode does not supervise them. Errors are `AdapterError { message }`
   worded for a trader.
7. **Keeping the simulator.** Sources, datasets and egress targets replace
   whole named objects across layers. A desk or user `sources.toml` entry
   named `[cvi]` with `adapter = "<real>"` replaces the demo's `[cvi]`, so
   `--demo` can run with one real source and every other family simulated.
   Without `--demo`, a desk layer must declare the datasets itself: the
   document, series and reference declarations, and the egress and
   positions documents, exist only in the demo layer. The builtin `cvi` and
   `dividend` panels are refused until `cvi_params` and `dividend_schedule`
   are declared.
8. **Testing.** Capture real messages as fixtures and run them through the
   kind's `parse` in `geode-documents` tests before connecting anything.
   Integration tests keep using `ChannelAdapter`, which implements
   subscription, recovery and egress against the same traits.

After any change to a stored dataset's columns, existing payload tables are
not migrated; open refuses the drifted dataset. Delete the table (or the
demo directory) and restart.

## Known gaps

- No vendor client crate exists, and non-demo startup registers no
  adapters: only `csv_dir` sources work without `--demo`.
- Directory readiness requires Geode's JSON `.done` sentinel;
  `stable_mtime` readiness is not implemented.
- XML wire tags are code constants, not config.
- Reference sources cannot rename columns (`source_name` is ignored).
- No document kinds for repo curves, correlation / term structure / skew,
  index compositions or instrument documents; no seam for broker quotes,
  trade history or sales credits beyond the existing traits.
- Option chains have only the subscribed-document path. A chain served by
  query (KDB, Bloomberg) would need a shim that produces document bytes
  per (underlying, expiry), or a new seam.
- Recovery only re-asks known topics; there is no on-demand fetch of a
  document key never seen before.
- No worker imposes a timeout on adapter calls; a hung shim call holds its
  worker and can delay shutdown.
- Upload success is only the adapter's return; confirmation relies on the
  echo assumption above.
- Expired option-chain expiries are never retired from live storage.
- Only `MockPricer` and `DemoVolModel` exist; `cvi_reanchor` and
  `cvi_recalc_forward` refuse; the pricer's health chip never shows because
  no source feeds a dataset the pricer reads.
- Subscriptions run while the app is closed only once the background
  collector is installed (`geode-collector install`); without it recovery
  on subscribe is the only gap filler, and with it recovery still covers
  each handoff gap.
- Nemo URL shapes are provisional.
- No KDB history client exists; `demo_hanweck` answers one close per
  weekday and intraday points for the current day only, so `step` and
  intraday history are untested against a real source.
