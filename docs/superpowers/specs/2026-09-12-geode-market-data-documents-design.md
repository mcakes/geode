# Geode — Market-Data Documents Design (roadmap slice 1)

**Date:** 2026-09-12
**Status:** Approved in brainstorm; pending written review
**Governs:** the document dataset family, the subscribed-document
adapter shape and its simulator, the document-kind parser crate, the
document request, the market-data panel with its editable draft, and
the egress request — built end to end for CVI parameters first.
Slice 1 of `docs/superpowers/specs/2026-09-12-geode-modules-roadmap.md`,
whose rulings 1, 3, 4, 5, 6, 7, 8 and 9 this document implements.
Conforms to the foundation design and `docs/PHILOSOPHY.md`.

## 1. Scope

### 1.1 What slice 1 delivers

- A second dataset family, **document**, declared in `datasets.toml`
  beside the existing measure datasets: an identity key, ordered axes,
  value columns and document-level attributes (§3).
- Storage and publication of received documents as generations, with
  as-of, retention, health and the freshness catalog carrying over
  unchanged (§4).
- An **adapter tier** in the data crate: a subscription trait, an
  egress trait, an adapter registry the app fills, an in-process
  channel adapter that is both the test fixture and the demo bus, and
  a per-key latest-wins coalescer (§5).
- A pure **document-kind crate** holding one typed parser and writer per
  document kind, CVI first (§6).
- A **document request** on the data handle answered through the
  existing query outcome route (§7).
- A **market-data module crate** hosting one panel implementation
  parameterised by a spec, registered as one roster kind per document
  kind, with a cursor, cell editing, a draft over the received document,
  rebase and discard, and session persistence (§8).
- **Egress**: `:upload` with a confirm, the upload request, the
  outcome delivered to the tile, and the draft clearing on a matching
  echo (§9).
- Two shell changes that ride along: module-shipped keymap fragments,
  and the tile delivery route growing from a query outcome to an enum
  (§8.6).
- The demo generator for CVI documents and the demo flag booting the
  bus (§10).

### 1.2 Done state

`cargo run -p geode-app -- --demo` boots with the CVI dataset and a
demo-bus source declared in the compiled-in demo layer. The palette's
"CVI: Split" adds a panel; `:key SPX.Z` shows that underlying's node
ladder across the top, its terms down the side, the params in the
cells, and anchor date, spot ref, generation time and staleness in the
header. The bus republishes on its schedule and the panel follows.
`i` on a cell opens an input, enter commits, the cell paints as edited,
and `:upload` prompts, sends, and the draft clears when the echoed
document arrives. A publish landing under an open draft marks the
draft as based on an older generation and offers `:rebase` and
`:discard`. Every behaviour has a test, every data-tier behaviour a
mutation entry, and the parse-plus-publish cost per document and the
panel's paint cost at the largest sketched matrix are in `docs/perf.md`.

### 1.3 Explicitly not in slice 1

- Link groups, launch with context, and any coupling between the
  blotter's cursor and a panel (roadmap slice 2). A panel's key is set
  by `:key` and the catalog picker only.
- Axes on measure datasets and the scenario panel (slice 2).
- Any panel beyond CVI. Repo, dividends, correlation and index
  compositions are each a generator, a document kind, a dataset
  declaration and a panel spec on the rails this slice builds, and
  follow as their own small tasks.
- The vendor Solace crate. Its trait is fixed here (§5.2); the crate is
  written on the work machine against the XSDs and the real bus
  (roadmap ruling 5).
- A general row query over a view. §7's document request is by key.
- Execution-class safety around uploads (roadmap ruling 4).

## 2. Amendments to earlier designs

- **Foundation §5.1 "Source"** gains an adapter name; "day one ships one
  adapter" becomes "the directory adapter is the default and the
  registry holds the rest" (§5.1).
- **Foundation §2 dependency rules:** "no crate other than `geode-data`
  may open a file or socket" becomes "no crate outside the data tier —
  `geode-data` and the adapter crates it registers — may open a file or
  socket" (roadmap ruling 3).
- **Phase 2 §3.2 "grain is the organizing principle"** now reads "for
  the measure family". A document dataset has no grain (§3.1).
- **Phase 2 §4.3 "the publication unit is the file"** stands, with a
  received message being a file whose batch is its key (§4.1).
- **Phase 3 §3.1 `TileContent::deliver`** takes a `Delivery` enum
  rather than a bare `QueryOutcome` (§8.6).
- **Phase 3 §3.2 the roster:** `ModuleFactory` gains
  `default_keymap` (§8.6); the reserved action lists in
  `geode_shell::defaults` are retired once every module ships its own.
- **Phase 2 §6.3 `ScopeSemantics`** gains a third variant,
  `NotApplicable { dimensions }` (§3.4).

## 3. The document family

### 3.1 Declaration

A dataset declares its family. Absent, it is `measures` and nothing
about the existing datasets changes.

```toml
[cvi_params]
family = "document"
key = ["underlying_ref"]          # one document per key value
axes = ["term", "node"]           # ordered; identify a row within a document

[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"                # document-level: constant within one document
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
```

`DatasetSpec` gains `family: Family` (`Measures | Document`), and for
the document family `key: Vec<String>` and `axes: Vec<String>`.
`ColumnRole` gains `Axis` and `Value`. `Attribute` keeps its name and
takes a document-level reading on this family: one value per document,
repeated on every row of it.

A pair dataset (correlation, later) lists two key columns. The key is
the batch (§4.1), so its values are joined with a separator that no
dimension value may contain — `\u{1f}` — and the catalog splits them
back.

### 3.2 Validation (`validate_dataset`)

Checked at load, every failure a diagnostic with `Diagnostic.path`
set to the offending key:

- `family = "document"` requires a non-empty `key` and a non-empty
  `axes`; each named column must exist. Error, dataset dropped.
- Every key column has `role = "dimension"`. Error, dataset dropped.
- Every axis column has `role = "axis"`; every `role = "axis"` column
  is listed in `axes`. Error, dataset dropped.
- Every value column is `f64` or `i64`. Error, column dropped.
- `grain = …` on any column, or a `measure` or `key` role, is an error
  on a document dataset: the column is dropped and the diagnostic
  names the family. `axis` and `value` on a measure dataset are the
  mirror error. A mixed declaration is refused, never guessed at.
- A document dataset declares at least one value column. Error,
  dataset dropped.
- `textual` and `categorical` keep their existing rules; a document
  dataset's `utf8` dimensions default to categorical like any other.

### 3.3 What the rest of the schema sees

- `groupable_columns` and the dimension picker's roster include a
  document dataset's dimensions. Its axes are never offered: they are
  row identity inside a document, not something the frame groups by.
- `grains()` is empty for a document dataset; every caller that
  iterates grains already handles an empty list.
- A document-level attribute cannot vary within one document by
  construction: `DocumentRows` carries one `Value` per attribute and
  `publish_document` writes it onto every row (Part 1 ruling). The
  §3.5 conflict detector therefore has nothing to detect here and is
  not run on document tables.

### 3.4 Shared dimensions and scope applicability

Shared dimensions are by name and role only. A scope selection on
`underlying_ref` applies to every dataset declaring a dimension of that
name, regardless of family. When a scope carries a dimension a dataset
lacks, the query drops that selection for that dataset and the
snapshot's provenance records `ScopeSemantics::NotApplicable {
dimensions }` naming the dropped columns, beside the existing `Direct`
and `SemiJoined`. A tile paints the marker the same way it paints an
unscoped one: an unscoped number is never misread, and neither is one
the scope could not reach.

Slice 1 uses this only in the document request (§7), which carries no
scope. The rule is stated here so slice 2's link groups and the
picker inherit it rather than invent it.

## 4. Storage and publication

### 4.1 One table, long form, message is file

One table per document dataset with the declared columns, dimension
columns ENUM-encoded as today, plus the same per-file bookkeeping the
measure tables carry. A received message is a file whose batch is the
document's key values (§3.1) and whose book is empty. `publish_file`'s
sibling `publish_document` takes parsed rows rather than a CSV path
and runs the same transaction: stage, validate against the schema,
swap into live, copy the previous live into the archive, bump the
generation, update `file_generations`, `generations` and the freshness
catalog. Per-batch generations, the live and archive pair, as-of
routing, retention and the generations summary table apply unchanged.

### 4.2 Source time

A source declares where its documents' time comes from:
`source_time = "receive"` (the adapter's receive stamp) or
`source_time = "document:<field>"` (a document-level attribute of
`date` or datetime type, once an XSD shows one). The choice is
recorded in config, never inferred from the document. The backfill
guard (Phase 2 §4.4) applies: a document older than the live one for
its key is archived, not swapped in.

### 4.3 Health and freshness

Health stays per source through the two-lane tracker. Connection state
reports through the discovery lane (§5.3); a parse or publish failure
through the load lane, keyed by batch as today, leaving the last good
generation live. The freshness catalog gives last-received per key.

### 4.4 Retention

Per-dataset retention config applies to document batches unchanged. A
document source at a 500 ms coalesce can mint a generation every half
second per key; the demo layer's retention for `cvi_params` is set by
count so the archive stays bounded, and the spec's perf section
records the archive growth rate at the demo cadence.

### 4.5 As built (Part 1)

- A document table carries a `book VARCHAR` column, always NULL, between
  `batch` and `source_file_id`, because every partition-keyed store
  statement joins on `book`.
- A document dataset's `Dimension` columns must all be listed in its
  `key`; an unkeyed dimension is an error at load and the column is
  dropped.
- `DocumentRows::validate` refuses a document with zero rows, so no
  generation is ever recorded against an empty table.
- The document request's as-of arm pins the document's own resolved
  generation and reports `resolved_as_of` as that document's own
  `source_time`, never the dataset-wide fold; its live arm's freshness
  is likewise per document, not per dataset.
- A column named `book` is refused on this family (error at
  `{ds}.columns.book`, dataset dropped). `create_document_table_sql`
  appends its own `"book" VARCHAR`, so the DDL would carry the name twice
  and `DataService::open` would fail on it, taking every other dataset
  with it. `RESERVED_COLUMNS` cannot carry the rule — `book` is a legal
  grain key column on the measure side — so it is per family, checked
  after `validate_document`'s own column drops so it fires only on a
  column that would really reach the DDL.
- `geode_core::document::Column` and `Value` cover **f64, i64, utf8 and
  date only**. `validate_document` therefore refuses `timestamp` and
  `bool` on an axis or an attribute (error at
  `{ds}.columns.{name}.type`, column dropped — dropping an axis leaves
  `axes` naming an undeclared column, so the dataset goes too; a *value*
  is already held to the stricter f64/i64 rule and is not reported
  twice), and requires every key column to be `utf8` (error at
  `{ds}.key`, dataset dropped: the key is joined into the `batch
  VARCHAR` column by `join_key` and bound back as `Value::Text`, so any
  other declared type compiles and selects nothing). Widen the refusal
  when — and only when — Part 2 widens `Column`/`Value`.
- The catalog's `live_rows`/`archive_rows` are summed over
  `store::ddl::table_pairs(ds)`, not `ds.grains()`: a document dataset
  has no grain and reported 0 rows beside a real list of live
  partitions.
- The picker's distinct-values query has a document arm
  (`query::distinct::document_select`, spec §3.3): one union branch over
  the document pair, shaped exactly like the measure arms' so the
  union's column set agrees. Live reads the live table; as-of reads both
  sides under `generation_predicate(&resolve_generations(conn, ds, t))`
  resolved **dataset-wide**, because distinct spans every key by
  definition — the narrowing `compile_document` does to one batch would
  be wrong here. Only `Dimension` columns are offered, never an axis
  (§3.3) and never a value.
- **The whole scope reaches that arm, lowered grain-free** (Part 2 Task 1,
  closing the item Part 1 parked): `Scope::applicable_to`'s kept dimension
  selections through `scope_sql::selection_clause` (extracted so the
  derived-value translation and the `string_split` binding form are
  spelled once), the text filter through `scope_sql::text_column_term`
  (extracted from `compile_scope_cached`'s own text block, so the
  dictionary `IN` for a categorical column and the row-scanning `ILIKE`
  for everything else are likewise spelled once), and the expression
  filter through `scope_sql::render_expr`, one top-level `and` conjunct at
  a time. `compile_scope` is still the only place a *grain* routes any of
  them — `route`, `evaluable_at`, `membership`, `Era::relation` — but
  routing only ever decides *where* a term is evaluated, and a document
  dataset is one table, so every one of those steps is a no-op here. Two
  rules carry over from the measure path because the direction of the
  error matters more than the rule: a text filter with no surviving term
  (every dictionary dropped the needle, or the dataset declares no textual
  column at all) compiles to a literal `false`, because a needle over a
  dataset that cannot be searched matches nothing, never everything; and a
  selection or expression conjunct naming a column the dataset has no
  storage for is **dropped** — neither compiled, since a binder error
  would fail the whole picker query on the one dataset with no `book`, nor
  collapsed to `false`, which would claim the document holds no such rows
  rather than that the question never reaches it. The widening that leaves
  is the disclosed one `ScopeSemantics::NotApplicable` exists to report,
  once the panel produces it.
- **No query produces `ScopeSemantics::NotApplicable` yet.** The type
  and `Scope::applicable_to` exist, and the blotter paints the marker,
  but the one caller that drops selections today (`document_select`) has
  nowhere to report them: `DistinctOutcome` is `{key, tag, column,
  values}` — `(value, count)` rows and no provenance or per-column
  semantics at all. Part 2's panel is where the marker starts being
  produced.
- `compile_document` does **not** cast the key column to its ENUM type,
  the way the view compiler casts a dimension for dictionary encoding
  (§7.2). The type is maintained — `publish_document` calls
  `refresh_enum` for each key dimension — so the cast is available; it
  is a Part 2 **measurement** item rather than a correctness one, since
  a document request returns one key's worth of rows rather than a
  million.
- `publish_file` moves rows **positionally** (`insert into {live}
  select *, …` from staging; the outgoing generation `select *` into the
  archive), and both table shapes emit their payload columns in TOML
  file order. Reordering two same-typed columns in `datasets.toml`
  against an existing database therefore misfiles their values with no
  error — the same reason CLAUDE.md's `--demo` note says to delete the
  database after a column change, now including a reorder.

## 5. The adapter tier

### 5.1 Config

```toml
[sources.cvi]
adapter = "solace"                # "demo_bus" in the demo layer; "csv_dir" is the default
dataset = "cvi_params"
document = "cvi_params"           # the document kind (§6) the parser handles
topics = ["marketdata/cvi/>"]
coalesce = "500ms"                # publish at most this often per key; "0" publishes every message
source_time = "receive"
priority = "latest_other"
```

`SourceSpec` gains `adapter: String` (default `csv_dir`), and for
subscribed adapters `document: String`, `topics: Vec<String>`,
`coalesce: Duration` and `source_time: SourceTime`. `paths`,
`readiness`, `poll_interval` and `pending_timeout` are the directory
adapter's and are a warning when given to a subscribed one; `topics`
and `document` are an error when missing from one. `parse_duration`
grows a `ms` unit.

### 5.2 Traits

In `geode_data::adapter`:

```rust
pub struct Message { pub topic: String, pub received: DateTime<Utc>, pub bytes: Vec<u8> }

pub trait Subscription: Send {
    /// Deliver every message matching `topics` to `sink` from the
    /// adapter's own thread. Must never block on the sink: a refused
    /// push is the sink's to count. Connection state goes to `health`.
    fn subscribe(&mut self, topics: &[String], sink: MessageSink, health: HealthSink) -> Result<(), AdapterError>;
    fn unsubscribe(&mut self);
}

pub trait Egress: Send {
    /// Send `bytes` to `target`. Called on the adapter's thread; the
    /// result is reported through the outcome, never by blocking a caller.
    fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError>;
}

pub trait Adapter: Send {
    fn name(&self) -> &'static str;
    fn subscription(&self) -> Option<Box<dyn Subscription>>;
    fn egress(&self) -> Option<Box<dyn Egress>>;
}
```

`MessageSink` wraps a bounded `sync_channel` and counts refusals the
way `EventSink` does. `HealthSink` carries `Connected`, `Reconnecting`
and `Lost { reason }`. That is the whole surface a vendor crate
implements; parsing, coalescing and publishing never see the vendor.

### 5.3 Registry

`DataServiceConfig` gains `adapters: AdapterRegistry`, a name-keyed
list the app fills — the module roster's pattern. `DataService::open`
resolves every source's `adapter` name against it. A name the registry
lacks marks the source `Health::Failed` with reason
"adapter 'solace' is not in this build", reported through the
discovery lane before the scheduler starts, and the rest of the
sources load. The directory adapter is registered unconditionally and
is what every existing source resolves to.

Vendor crates (`geode-adapter-solace`, later) sit beside `geode-data`,
depend on it for the traits, are registered by `geode-app` behind a
cargo feature of the same name, and are never enabled in CI.

### 5.4 The subscription pipeline

For each subscribed source `DataService::open` builds one
`Subscription`, hands it a sink and a health sink, and subscribes.
A dedicated receiver thread per source drains the sink and runs:

1. **Parse** through the source's document kind (§6). A failure is a
   load-lane `Failed` for that key with the parser's message; the
   message is dropped and the last good generation stays live.
2. **Coalesce** per key: the latest parsed document replaces any
   pending one (whole-document replacement, roadmap ruling 1, makes
   latest-wins correct). A key is released to the runner when
   `coalesce` has elapsed since its last release, or immediately at
   `coalesce = "0"`. The coalescer is a pure struct with a `now`
   parameter, tested without threads.
3. **Enqueue.** *Amended (Task 11, §5.6): as built, this is not a
   `WorkItem`/`Candidate::Document` variant riding the source's
   `priority` alongside files on one rung — it is a second, separate
   queue (`Queue::documents: VecDeque<DocumentJob>`) the runner always
   pops from first, ahead of any file whatever that file's `Priority`.
   A subscribed source's own `priority` key is still parsed like any
   other source's but has no effect on a document's queue position; a
   `DocumentJob` carries no `Priority` field at all.* `source_time` is
   still per §4.2. The existing ingest runner pops it and calls
   `publish_document`.

Connection state from the health sink is forwarded through
`report_discovery_and_emit`: `Connected` is `Ok`, `Reconnecting` is
`Pending`, `Lost` is `Failed` with the reason. The load lane is
untouched by any of these, so a reconnect can never clear a bad
document.

### 5.5 The channel adapter

`geode_data::adapter::channel::ChannelAdapter` is an in-process
adapter with no sockets, compiled unconditionally. Its subscription
side is fed by a `Sender<Message>` the constructor hands back; its
egress side pushes uploaded bytes back into its own inbound channel
under the topic the target names, after the writer, which is the echo
§9.4 relies on. It is the fixture every data-tier test uses, and the
demo bus's transport (§10).

### 5.6 As built (Part 2)

- **Documents are popped ahead of files, not merged into the file
  queue's own priority rung.** §5.4 step 3 above is amended in place:
  the runner's `Queue` gained a second, separate `documents:
  VecDeque<DocumentJob>` always taken before `items` (the
  priority-ordered file queue), whatever the head file's `Priority` —
  a coalesced document publish is milliseconds, so popping it first
  cannot starve a file load, and `DocumentJob` carries no `Priority` at
  all. A subscribed source's `priority` config key is still parsed and
  stored like any other source's; it simply has no effect on a
  document's queue position. `IngestHandle::shutdown` drops whatever is
  still queued in `documents` exactly as it drops whatever is still
  queued in `items` — neither queue drains before the runner thread
  returns.
- **`SourceTime::Document(field)`'s two readable shapes, and where each
  is checked.** `SourceSpec::from_doc` validates `sources.<name>.
  source_time` at LOAD: the named field must be a document-level
  `Attribute` (§3.1) of type `date` or `utf8`, or the source is refused
  with an `Error` diagnostic and skipped. `source_time_of`
  (`geode_data::ingest::subscribe`) then reads, per document: a `Date`
  as that date's value at MIDNIGHT UTC (a business date has no time of
  day); a `Utf8` as RFC 3339, which carries its own offset, so a feed
  stamping local time with an offset is honoured rather than silently
  read as UTC. A missing or wrongly-typed field on an actual message is
  reported per document even though the schema check already ran at
  load — that check is about the declared shape, this one about what a
  given message actually sent.
- **The coalescer's window restarts from each release, and a key's
  first offer always releases at once.** `Coalescer<T>::offer`, on a
  key with nothing pending and nothing yet released, returns the item
  immediately; once released, the *next* release for that key is due
  no earlier than `window` after THAT release, not after the original
  offer — a re-offered pending item's own due time never moves, so a
  fast-repeating key cannot push its own release out forever, but a
  change arriving right after a release waits out a full fresh window
  before the next one shows.
- **`ConnectionState` maps onto the discovery lane exactly; parse,
  validate and `source_time` failures map onto the load lane, keyed by
  batch or, when the bytes never parsed far enough to yield one, by the
  raw topic.** `Connected → Health::Ok`, `Reconnecting →
  Health::Pending("reconnecting")` (nothing lost yet — a trader should
  read "waiting", not "broken"), `Lost { reason } → Health::Failed {
  reason }` (the adapter's own reason, verbatim) — all through
  `report_discovery_and_emit`. A parse failure has no key (the bytes
  never parsed), so it reports on the LOAD lane keyed by the message's
  raw topic; a validate or `source_time_of` failure has a key (the rows
  parsed fine) and reports keyed by that document's own batch. A
  panicking `DocumentKind::parse` runs under the same `catch_unwind` +
  `geode_core::panic::contained` boundary every other background
  boundary in this crate uses, and is reported exactly as a parse
  `Err` would be — keyed by topic, `"parse panicked: …"` — so one
  malformed message costs one document, not the receiver thread. The
  receiver's `handle_message` is this crate's SIXTH such boundary, not
  its fifth: an ingest file's load, its pop-time catalog recheck, a
  discovery poll, a query pool worker (Phase 4b's original four), this
  plan's own document publish (`publish_one_document`, Task 8), and now
  a message receive (Task 9).
- **`AdapterRegistry` and `DocumentRegistry` live on
  `DataServiceConfig` and are filled by the app, never by `geode-data`
  itself.** `geode_app::bridge::data_setup` always folds in
  `geode_documents::builtin_kinds()` into `documents` — a document kind
  carries no state, so there is nothing a caller could sensibly leave
  out — while `adapters` is the caller's own roster: `main.rs`
  registers a `ChannelAdapter` named `"demo_bus"` only under `--demo`;
  every other build passes `AdapterRegistry::default()`, so a
  non-demo build serves every `csv_dir` source and reports each
  subscribed one as unservable rather than silently doing nothing.
  `geode-data` depends on neither `geode-documents` nor
  `geode-demo-data` — confirmed in each crate's `Cargo.toml` — so the
  layering rule holds structurally, not just by convention.
- **A missing adapter, a missing document kind, a kind/dataset column
  mismatch, or an adapter with no subscription side is a discovery-lane
  `Failed` for that source, resolved once at `DataService::open` — and
  is NOT also surfaced as a `Diagnostic` in the config section.** All
  four resolution failures (§5.3, §6.4) go through the same
  `report_unservable` closure onto the discovery lane. `DataSetup::
  diagnostics` is built by `data_setup` from schema/view/dimension/
  source *parsing* alone, before `DataService::open` ever runs, so none
  of these reaches it. Recorded as a known gap, not fixed here: a
  trader sees the failure in the diagnostics tile's data section
  (as a source health), never its config section.
- **`ChannelAdapter` can lose a capability at runtime, and both its
  doors say so rather than pretending.** It holds its inbound sender
  only weakly, so the bus closes for good once every `ChannelFeed` and
  outstanding egress is dropped (no new strong sender can be made from
  a `Weak` with no strong holders left): `subscribe` on a closed bus is
  `Err`, never a `Connected` that then delivers nothing, and `egress()`
  answers `None` once every feed is gone. `ChannelFeed::refused()`
  counts inbound publishes the bus itself could not queue (the
  dispatcher fell behind); `SubscriptionWorker::refused()`
  (`geode_data::ingest::subscribe`) exposes a subscribed source's own
  dropped-message count the same way. Neither has an in-app reader yet
  — the natural next one is the diagnostics tile's sources section.
- **The demo bus publishes every key once at start, then advances one
  key per `cadence ± jitter` sleep.** `geode_app::demo_bus::spawn`
  publishes every one of `CviGenerator::underlyings()` immediately, so
  a panel opened at startup has something on its first frame, then
  loops over the keys forever, waiting `(cadence - jitter) +
  uniform(0, 2 * jitter)` before each publish. The overall rate of new
  generations is therefore about one every `cadence` (the shipped
  constant is 5s) regardless of key count — the bus advances one key
  per wait, not one wait per key — while any one key's own republish
  period is roughly `cadence × keys` (ten demo underlyings ⇒ about 50s
  per key at the shipped cadence).
- **The picker's document arm lowers the whole scope grain-free (Task
  1, closing the item §4.5's Part 1 bullet parked).** `query::
  distinct::document_select`'s text filter runs through
  `scope_sql::text_column_term` per textual column (a literal `false`
  when none survives), and its expression filter runs one top-level
  `and` conjunct at a time through `scope_sql::render_expr`. A
  conjunct naming a column the document table has no storage for is
  DROPPED — neither compiled (a binder error would fail the whole
  picker query on the one dataset with no such column) nor collapsed
  to `false` (which would claim the document holds no such rows rather
  than that the question never reaches it) — mirroring exactly the
  widening `Scope::applicable_to` already does for a dimension
  selection.
- **`--demo` feeds `cvi_params` for real now.**
  `geode_demo_data::documents::cvi::CviGenerator` (seeded; 8 listed
  monthly expiries × 12 fixed nodes per document — smaller than either
  of the two synthetic grid shapes `docs/perf.md`'s benchmarks use) and
  `geode_app::demo_bus` are the generator and producer, subscribed
  through a `[cvi]` source (`adapter = "demo_bus"`) the demo layer
  declares alongside the risk snapshot's own directory source. Numbers
  — parse/write, publish, and archive growth at the demo cadence — are
  in `docs/perf.md`'s "Market-data documents" section.

## 6. Document kinds

### 6.1 The crate

`geode-documents` is a pure crate: no I/O, no gpui, depends on
`geode-core` only. It holds one module per document kind, each
exposing a typed model, `parse(&[u8]) -> Result<DocumentRows,
ParseError>` and `write(&DocumentRows) -> Result<Vec<u8>, WriteError>`.
`geode-data` and `geode-demo-data` both depend on it; neither depends
on the other, which is why the trait below lives in `geode-core`
rather than in the data crate (the data crate dev-depends on the demo
generator).

### 6.2 The vocabulary (`geode_core::document`)

```rust
pub struct DocumentRows {
    pub key: Vec<String>,                          // in the dataset's `key` order
    pub attributes: Vec<(String, Value)>,          // document-level, one value each
    pub axes: Vec<(String, Column)>,               // one column per axis, all the same length
    pub values: Vec<(String, Column)>,             // one column per value column, same length
}

pub trait DocumentKind: Send + Sync {
    fn name(&self) -> &'static str;
    /// The columns this kind produces, checked against the dataset it
    /// feeds when the source opens (§6.4).
    fn columns(&self) -> &[(&'static str, ColumnType)];
    fn parse(&self, bytes: &[u8]) -> Result<DocumentRows, ParseError>;
    fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError>;
}
```

`Column` is struct-of-arrays: `F64(Vec<f64>)`, `I64(Vec<i64>)`,
`Utf8(Vec<String>)`, `Date(Vec<NaiveDate>)`. The CVI parser produces
`term` and `node` axes of length terms × nodes, in term-major order,
and one `param` value column.

### 6.3 CVI

Hand-written over `quick-xml` now; regenerated from the XSD behind the
same two functions later. The model follows the document as described:
`marketData/underlying`, `cviParams/anchorDate`, `cviParams/spotRef`,
`cviParams/nodes/node*`, `cviParams/slices/slice*` each with `term`
and `param*`. Parsing rules:

- A slice whose `param` count differs from the `node` count fails the
  message with both counts in the error. Positional alignment is the
  whole point; a ragged document is not guessed at.
- An element the model does not know is skipped and logged once per
  (source, path) at `warn` under `geode::ingest`; the set of skipped
  paths is a per-source diagnostic so an XSD drift is visible.
- A missing required element (`underlying`, `anchorDate`, `nodes`,
  `slices`, or any `term`) fails the message.
- `write` emits the same shape from `DocumentRows`, so that generated
  → written → parsed round-trips exactly (§11's property test), and
  so an upload sends what the panel showed.

### 6.4 Registration

`DataServiceConfig` gains `documents: DocumentRegistry`, name-keyed,
filled by the app. When a source opens, the service checks the kind's
`columns()` against the dataset's declared axes, values and attributes
by name and type; a mismatch is one load-time error diagnostic naming
both sides, and the source is not subscribed. A source naming a kind
the registry lacks is the same failure as a missing adapter.

## 7. The document request

```rust
pub struct DocumentParams {
    pub key: QueryKey,            // the tile's, as for every query
    pub tag: u64,
    pub dataset: String,
    pub document_key: Vec<String>,
    pub as_of: AsOf,
}
```

`Request::Document(DocumentParams)` runs on the query pool with the
same cancellation and coalescing as `Request::Query`. The compiled
statement selects the dataset's axes, values and attributes from the
live table, or from the archive generation `resolve_generations`
picks under `as_of`, where the key columns equal `document_key`,
ordered by the axes in declared order. The result is an ordinary
`QueryOutcome` whose `Snapshot` carries the rows and a `Provenance`
with that batch's generation, source time and health — delivered
through the tile route unchanged. A key with no live row yields an
empty snapshot with provenance saying so, which the panel paints as
"no document received for SPX.Z", not as an empty matrix.

Which keys exist, for the panel's `:key` completions, is the existing
catalog: a document dataset's partitions are its keys, one per batch,
with generations. The panel reads them from the shell's `Diagnostics`
entity (its factory already receives it), asking for a refresh through
`Diagnostics::request_catalog()` and calling `cx.notify()` in the same
update block — the documented trap: the request is queued but unseen
until something notifies. No new request on the data handle.

## 8. The panel

### 8.1 Crate and roster

`geode-marketdata` hosts one `MarketDataTile` parameterised by a
`PanelSpec`, and one `ModuleFactory` per panel spec, so the roster
kind is `cvi` and the palette reads "CVI: Split". Specs are code in
slice 1 (one, for CVI); making them config is a later task once a
second panel shows what varies.

```rust
pub struct PanelSpec {
    pub kind: &'static str,           // "cvi"
    pub title: &'static str,          // "CVI"
    pub dataset: &'static str,
    pub document: &'static str,       // the document kind for `:upload`
    pub rows: &'static str,           // the axis down the side ("term")
    pub columns: Columns,             // Axis("node") pivots; Values lays value columns flat
    pub header: &'static [&'static str],   // attributes shown in the header
    pub format: ColumnFormat,
}
```

### 8.2 What it paints

A header row: key, each header attribute, the generation's source
time, the staleness style the blotter uses, and the draft state
(§8.4). Below it, column labels across the top, row labels down the
side, and cells — painted with plain gpui elements from a
`MatrixModel` built once per snapshot or draft change and cached, so a
frame paints from prepared strings and never formats. The panel is
not virtualised (roadmap ruling 6); §11 records its paint cost at the
largest matrix the sketch implies so the decision is measured.

### 8.3 Keys

Context `marketdata` with `mode = normal | insert`, opted into counts.
Normal mode: `h j k l` move the cursor by cell with counts, `0`/`$`
first and last column, `gg`/`G` first and last row, `y` yanks the
cell, `yy` the row, `yc` the column, in the blotter's tab-separated
spelling. `i` or `enter` opens a cell input in place (insert mode:
enter commits, escape cancels, the value is parsed to the column's
type and a refusal shows inline). `/` finds a row or column label
through the shared find line. `:` commands: `key <value>` (completions
from the catalog's keys), `revert`, `bump <delta> [row|col]` (adds
`delta` to every cell in the cursor's row or column, default row),
`upload`, `rebase`, `discard`. `rebase` and `discard` are completions
only while a newer generation sits under the draft.

The factory registers `marketdata::*` actions and ships its default
bindings as a keymap fragment (§8.6).

### 8.4 The draft

`Draft { base: i64, edits: BTreeMap<(usize, usize), f64>, state }`
over the received snapshot, where `base` is the `gen_id` (from the
snapshot's provenance) the edits were made against. A cell with an edit paints in the edited
style and the header reads "3 edits on 14:02's document". States:

- **Clean** — no edits.
- **Editing** — edits present, base is the live generation.
- **Behind** — edits present and a newer generation has been
  delivered. The panel keeps painting the *base* generation under the
  edits (a newer document never clobbers a draft, roadmap ruling 9),
  the header says "newer document received 14:07", and `:rebase`
  re-applies the edits by row and column *label* onto the new
  generation (a label the new document lacks drops that edit and says
  so), while `:discard` drops the edits and shows the new generation.
- **Sent** — `:upload` succeeded; edits are kept and painted as sent
  until §9.4 clears them.

Only value cells are editable. Axes and attributes are read-only in
slice 1; editing `spotRef` is a plausible later verb and is not built.

### 8.5 Session

`serialize` writes `key`, `edits` (as label pairs, so a restart onto a
newer generation restores into `Behind` rather than misaligning), and
`base` generation id. Unsent edits are work and survive a restart.

### 8.6 Shell changes

- **`Delivery`.** `TileContent::deliver` takes
  `Delivery::Query(QueryOutcome) | Delivery::Upload(UploadOutcome)`.
  The bridge routes `DataEvent::Upload` to `ShellView::deliver` the way
  it routes `DataEvent::Query`. The blotter, diagnostics, placeholder
  and recording occupants match on the enum; the compiler refuses an
  occupant that forgets the new arm.
- **Keymap fragments.** `ModuleFactory::default_keymap(&self) ->
  Option<&'static str>` returns a keymap TOML fragment. `build_keymap`
  merges every factory's fragment as one layer above the built-in
  shell keymap and below the desk and user layers, so a user override
  still wins. A fragment may only bind in contexts the module itself
  declares (`marketdata`, `blotter`, …): a binding in any other context
  is dropped with an error diagnostic, so a fragment can never shadow
  a shell binding or another module's. The blotter and
  diagnostics move their bindings into fragments in the same task, and
  `geode_shell::defaults::BLOTTER_ACTIONS`, `DIAGNOSTICS_ACTIONS` and
  the two `_DEFS` tables are deleted along with their mirror tests.
  `register_actions` runs before `build_keymap` already, so a
  fragment's actions are registered when the keymap resolves them.

## 9. Egress

### 9.1 Config

```toml
[egress.sophis]
adapter = "solace"                # or "demo_bus"
target = "marketdata/upload/cvi"  # adapter-specific address
```

An `egress` doc, one table per target name, resolved against the same
registry. A target naming an adapter without an `Egress` side is a
load-time error diagnostic.

### 9.2 Request and outcome

```rust
pub struct UploadParams { pub key: QueryKey, pub tag: u64, pub target: String, pub document: String, pub rows: DocumentRows }
pub struct UploadOutcome { pub key: QueryKey, pub tag: u64, pub target: String, pub result: Result<(), String> }
```

`Request::Upload` runs on the service thread: look up the document
kind, `write` the rows, hand the bytes to the target's `Egress` on
its own thread, and emit `DataEvent::Upload` with the result. Nothing
blocks the UI; a refused event is counted like every other.

### 9.3 The confirm

`:upload` arms a confirm in the panel's command line — "upload 3
edited cells of SPX.Z to sophis? (y/n)" — taken by `y`, cancelled by
any other key, and disabled with a notice when the draft is clean or
`Behind` (rebase or discard first: an upload must be of a document the
trader has seen whole). On `y` the panel logs one `info` line under
`geode::ingest` naming the key, target and edit count, and submits
the request with the draft applied to its base rows.

### 9.4 The echo

On `Ok` the draft enters `Sent`. When the next generation for that key
is delivered and its values equal the sent rows (exact for `i64`,
within one ULP for `f64`, since the writer and the upstream echo
should agree bit for bit), the draft clears and the header says
"uploaded 14:09, confirmed 14:09". If the echo differs, the draft
stays `Sent`, the differing cells paint as such, and the header says
so — the trader decides. On `Err` the draft returns to `Editing` with
the error inline. The channel adapter echoes by construction (§5.5),
so the demo shows the whole loop.

## 10. Demo

- `geode-demo-data` gains `documents::cvi`: a seeded generator
  producing a `DocumentRows` per underlying for a configurable list
  (default: the risk generator's own underlying vocabulary), a fixed
  node ladder (`-20 … 3.5` in the sketch's spelling), a term ladder of
  listed expiries off the anchor date, and params that drift by a
  seeded random walk between publishes so successive documents differ
  visibly. Same seed, same documents (`same_seed`-style test).
- `geode-app::demo` registers a `ChannelAdapter` under the name
  `demo_bus`, and a scheduling thread that writes each underlying's
  next document through the CVI kind's `write` and pushes it on the
  channel at a configurable cadence with jitter (default 5 s ± 2 s per
  key). The compiled-in demo layer declares `cvi_params`, a
  `[sources.cvi]` on `demo_bus`, and `[egress.sophis]` on `demo_bus`.
- The demo database directory rule stands: a `datasets.toml` change
  means deleting `$TMPDIR/geode-demo/<rows>-<seed>`.

## 11. Tests, harness and benchmarks

Weighting per foundation §10.3: data tier ≫ shell ≫ module.

**Schema (`geode-core`).** Every rule in §3.2 has a test that loads a
doc violating it and asserts the diagnostic's severity, path and
outcome (dropped dataset or column). `groupable_columns` includes a
document dimension and excludes its axes. Mutation entries: the
family gate, the key-must-be-dimension rule, the axis listing rule,
the mixed-declaration refusal, the not-applicable scope marker.

**Storage (`geode-data`).** `publish_document` live then archive; a
second publish for the same key archives the first; as-of resolves
the older; the backfill guard refuses an older `source_time`;
retention by count over document batches; the conflict detector
flags a varying document-level attribute. Mutation entries for each.

**Adapter tier.** A `ChannelAdapter` test fixture. Coalescer: latest
wins within the window, release on elapse, `0` releases immediately,
two keys coalesce independently. Health: `Lost` reports `Failed` on the
discovery lane and a later `Connected` clears it; a parse failure sets
the load lane and a later clean document clears it; a `Lost` never
clears a load-lane `Failed` (the two-lane invariant, already an entry —
extended). A missing adapter and a missing document kind each mark the
source failed with the stated reason and leave other sources loading.
Column-list mismatch is one diagnostic and no subscription.

**Documents (`geode-documents`).** A property test: generated rows →
`write` → `parse` equals the input. Ragged slice fails with both
counts. Unknown element skipped and reported once. Each missing
required element fails.

**Request.** Document by key live and as-of, axis order, empty
snapshot with provenance for an unknown key, cancellation of a
superseded request.

**Panel (`TestAppContext`).** Cursor and counts; `i`, commit, cancel;
the edited style; `bump` on row and column; `Behind` on a publish
under a draft and the newer generation *not* painted; `rebase` by
label with a dropped label reported; `discard`; session round trip
into `Behind`; the confirm's `y` and cancel; `Sent` then clear on a
matching echo; `Sent` kept on a differing echo; the disabled upload
on a `Behind` draft. A harness entry for every `stop_propagation` on
a cell click.

**Shell.** `Delivery::Upload` reaches the addressed tile and no other;
a fragment binding resolves for a module action; a user layer
overrides a fragment; a fragment cannot shadow a built-in shell
binding; the reserved lists are gone (compile-time).

**Benchmarks (`docs/perf.md`, new section).** Parse plus publish per
CVI document at the sketch's size (order of 20 terms × 30 nodes) and at
ten times that; archive growth per hour at the demo cadence; the
panel's `MatrixModel` build and its paint at 20×30 and at the largest
matrix the scenario panel will need (measured now so slice 2 inherits
a number, not a guess).

## 12. Sequencing

Four mergeable parts, each reviewed and merged before the next starts.

1. **Data model and request** — §3, §4, §7, all headless: family,
   validation, `publish_document`, the not-applicable marker, the
   document request, the catalog listing keys. Mutation entries land
   with each behaviour.
2. **Adapter tier and documents** — §5, §6, §10's generator: traits,
   registry, channel adapter, coalescer, receiver pipeline, the
   `geode-documents` crate with CVI, the demo generator, registration
   in the app. `--demo` publishes documents into the database with no
   panel yet; the diagnostics tile's data section shows them.
3. **Panel and shell changes** — §8: the crate, the spec, the matrix
   model, keys, the draft with `Behind`/`rebase`/`discard`, session,
   `Delivery`, keymap fragments, the reserved-list deletion.
4. **Egress** — §9: config, request, outcome, confirm, `Sent` and the
   echo.

## 13. Open questions

- **Document timestamps.** Whether real documents carry a time field
  decides `source_time` for the real source; the demo uses `receive`.
  Waits for the XSDs.
- **Topic naming.** The real bus's topic structure and whether one
  subscription can carry several document kinds. The adapter trait
  takes a topic list and the source names one kind; a multi-kind
  topic would need a kind-detection step that is not designed here.
- **Editing axes and attributes** (`spotRef`) — plausible, not built.
- **Panel specs as config** — once a second panel exists.
- **Per-key retention** — a chatty key under a count-based policy
  starves nothing today (retention is per batch), but the archive
  growth number in §11 decides whether an age-based default is needed.
