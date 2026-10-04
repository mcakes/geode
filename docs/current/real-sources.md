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
   `demo_kdb`, `demo_rest`, `demo_refdb` and `demo_positions`; a duplicate
   registration replaces the earlier one with a warning.
5. **Credentials.** Read secrets from the environment. Config may name the
   variable, never hold the secret (see
   [credentials](configuration.md#credentials)).
6. **Contract checklist.** Fresh handle per capability call. `subscribe`
   and `recover` return promptly. Health callbacks are quick and never
   panic. Every blocking call (`fetch`, `catalogue`, `query`, `upload`,
   `move_lhu`) has its own timeout. Vendor callback threads are the shim's;
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
