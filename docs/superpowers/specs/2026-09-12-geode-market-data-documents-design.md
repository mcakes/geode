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
panel's model build and paint at 20×30 and at 10,000 rows are in `docs/perf.md`.

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
  Health::Pending` with the detail `"reconnecting"` (`Health::Pending`
  is a unit variant; the detail travels beside it, not inside it) —
  nothing lost yet, and a trader should read "waiting", not "broken" —
  `Lost { reason } → Health::Failed { reason }` (the adapter's own
  reason, verbatim) — all through `report_discovery_and_emit`. A parse
  failure has no key (the bytes never parsed), so it reports on the LOAD
  lane keyed by the message's raw topic; a validate or `source_time_of`
  failure has a key (the rows parsed fine) and reports keyed by that
  document's own batch. **A topic-keyed failure is cleared by the
  receiver itself, and by nothing else** (final fix wave): the load lane
  is per-batch and worst-across-batches, and its only other `Ok` writer
  is the ingest sink's `Published` arm, keyed by the document's own batch
  — `SPX.Z`, a different string from `marketdata/cvi/SPX.Z` — so the
  topic entry would otherwise stand `Failed` for the rest of the session
  while the source published perfectly good documents.
  `Receiving::failed_topics` (a `HashSet<String>`) remembers the topics
  filed that way, and the first message from one that parses, validates
  and stamps reports `Health::Ok` under that topic (detail `"{topic}:
  parse ok"`, normalised away by `Lanes::combined` as every clean slot's
  detail is) and forgets it. The tracker's own transition dedupe keeps
  emits to real recoveries, and a clean message on a topic that never
  failed costs one set lookup and no report. The closure the receiver is
  given is therefore a LOAD REPORT sink (`LoadReportSink`, `(batch,
  Health, detail)`), not a parse-failure one: this module reports both
  directions of the lane it writes. A
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
  dropped-message count the same way — by holding the shared COUNTER
  (`MessageSink::refused_counter`) and never a `MessageSink` clone,
  because a sink is a sender: with one alive on the worker's side
  `unsubscribe` disconnected nothing, the receiver loop's `Disconnected`
  arm was unreachable on shutdown, and every join waited out a `MAX_WAIT`
  tick, serially per source in `DataService::shutdown` (final fix wave).
  Neither count has an in-app reader yet — the natural next one is the
  diagnostics tile's sources section.
- **A subscribed source is described as one, and its `paths` is dropped
  rather than stored** (final fix wave). `SourceSpec::from_doc` already
  warned that a subscribed source's `paths` is ignored; it now clears the
  vector too, since a reader must not store what it has just said it
  ignores. `geode_shell::diagnostics::SourceSummary` carries `adapter`
  and `topics` (empty for a directory source, which is how a reader tells
  the two apart), filled by `geode_app::bridge::attach` from the
  `SourceSpec`, and the diagnostics tile's sources section paints two
  rows per source either way: `path:` plus `adapter · priority ·
  readiness` for a directory source, `adapter:` plus `topics:` for a
  subscribed one — which has no path to poll and no readiness rule, so
  printing either described a market-data feed as a directory source with
  an empty path and a sentinel convention it has never used.
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
(§8.4). Below it, the body: **gpui-component's table
(`DataTable`/`TableState`) over this crate's own `MatrixDelegate`
(superseded 2026-09-14 — roadmap ruling 6's revised form, a gpui
`uniform_list` with a column strip per row, is what Part 3 shipped and
what the user's "visual unity" ruling replaced; §8.8 records the as-built
seam).** The table's column 0 is the row-label column, carrying the row
axis's own name and pinned left as the blotter pins its tree column;
then one column per value column. Every cell comes out of a
`MatrixModel` built once per snapshot or draft change and cached, so a
frame paints from prepared strings and never formats, and the component
lays out only the visible rows. The cursor row and column are kept in
view by the table's own selection (`set_selected_row`/`set_selected_col`,
which scroll non-strictly, so a cell already on screen never jumps).
Row height is fixed (one line of the data face at `Size::XSmall`); there
is no horizontal virtualisation to arrange, since no sketched document
has more than a few dozen columns. §11 records the model build and the
paint at 20×30 and at 10,000 rows × 5 columns — the dividend-schedule
shape that forced the 2026-09-13 revision — so the decision is measured.

### 8.3 Keys

Context `marketdata` with `mode = normal | insert`, opted into counts.
Normal mode: `h j k l` move the cursor by cell with counts, `^`/`$`
first and last column, `gg`/`G` first and last row, `y` yanks the
cell, `yy` the row, `yc` the column, in the blotter's tab-separated
spelling. `i` or `enter` opens a cell input in place (insert mode:
enter commits, escape cancels, the value is parsed to the column's
type and a refusal shows inline). Insert mode is a tile-owned
gpui-component `Input` painted in the cell, and it works only because
of the shell rule in §8.6: the shell's key handler sits on the window
root and sees every keystroke, so without that rule a typed `j` would
also move the cursor. `/` finds a row or column label
through the shared find line. `:` commands: `key <value>` (completions
from the catalog's keys), `revert`, `bump <delta> [row|col]` (adds
`delta` to every cell in the cursor's row or column, default row),
`upload` (Part 4; in Part 3 the command answers "upload is not built yet"), `rebase`, `discard`. `rebase` and `discard` are completions
only while a newer generation sits under the draft.

The factory registers `marketdata::*` actions and ships its default
bindings as a keymap fragment (§8.6).

### 8.4 The draft

`Draft { base: String, edits: BTreeMap<(usize, usize), f64>, state }`
over the received snapshot, where `base` is the document's source
time (the snapshot's `Provenance.datasets[0].as_of`, RFC 3339) the
edits were made against — not a `gen_id`: a live query's provenance
carries the dataset-wide latest generation, not the document's own
(Part 1 §4.5), while its `as_of` is per document (`live_source_time`),
so the source time is the identity a panel can actually compare. A
delivered snapshot whose `as_of` differs from `base` is a newer
generation. A cell with an edit paints in the edited
style and the header reads "3 edits on 14:02's document". States:

- **Clean** — no edits.
- **Editing** — edits present, base is the live generation.
- **Behind** — edits present and a newer generation has been
  delivered. The panel keeps painting the *base* generation under the
  edits (a newer document never clobbers a draft, roadmap ruling 9),
  the header says "different document received 14:07" (§8.7.24 — an
  as-of step back delivers an OLDER one into this very state), and
  `:rebase`
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
`base` (the source time string). Unsent edits are work and survive a restart.

### 8.6 Shell changes

- **`Delivery`.** `TileContent::deliver` takes a `Delivery` enum.
  Part 3 introduces it with the one variant `Query(QueryOutcome)` and
  routes it through `ShellView::deliver`; Part 4 adds
  `Upload(UploadOutcome)` and the bridge's `DataEvent::Upload` route.
  The blotter, diagnostics, placeholder and recording occupants match
  on the enum, so the compiler refuses an occupant that forgets Part
  4's arm.
- **Insert mode.** While the focused tile's key context carries
  `mode == insert` and window focus is on a handle the shell does not
  own (`holds_shell_focus` is false), `ShellView::handle_key_down`
  resolves only *single-keystroke* bindings against the context stack
  — the module fragment's `escape`/`enter` in `marketdata && mode ==
  insert`, and chords — and lets every other keystroke propagate to
  the focused input, exactly as the scope bar's filter field already
  does for itself; the matcher's sequence and count state is never
  fed. The tile drops its `InputState` on commit or cancel, and that
  dropped handle is what returns focus to the shell root through the
  existing `window.focused(cx).is_none()` net in `render` (superseded
  — see §8.7: the drop alone does not produce this at the pinned
  gpui-component rev, and an occupant must blur before dropping) — no
  module ever touches the shell's focus handle. A second module wanting text
  entry (the pricer) inherits the rule unchanged.
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

### 8.7 As built (Part 3)

1. **Insert mode is one branch, not a mode-wide takeover.**
   `ShellView::handle_key_down` (`shell/input.rs`) tests `mode ==
   insert` and `!holds_shell_focus` before the matcher; a bare key
   (`Modifiers::is_chord` false — shift alone is typing) resolves only
   against contexts that themselves carry `mode == insert`
   (`insert_contexts`), while a chord resolves against the whole
   stack. §8.6's text did not distinguish the two; without the split a
   module's bare insert-mode `escape`/`enter` would sit behind the
   shell's own bare bindings (`/`, `:`, `shift+d`) reachable from the
   rest of the stack, and a trader could not type those characters
   into a cell.
2. **An explicitly unbound chord in insert mode does not
   `stop_propagation`**, unlike the filter-field branch it otherwise
   mirrors, so `alt+letter` and any other unbound chord can still type
   into a cell editor — a deliberate divergence from the filter
   field's own rule, which claims every chord for itself.
3. **Dropping an `InputState` does not return focus by itself,
   superseding the mechanism §8.6 states** (left in place above with a
   pointer here, per the amendment). At the pinned gpui-component rev,
   `Root` registers the focused input as a strong `AnyInputState`
   (`input::state::sync_focused_input_registry`) and only ever
   unregisters it from that input's own render — which a removed
   input never reaches, so the last strong clone outlives the drop and
   `Window::focused` never reports `None`. An occupant must call
   `window.blur(cx)` (giving up focus, never taking the shell's own
   handle) and only then drop the entity; `MarketDataTile::close_editor`
   and `geode_shell::module::recording`'s test fixture both implement
   the sequence, and a future text-entry module owes it too.
4. **`Delivery` is `pub enum Delivery { Query(QueryOutcome) }` with
   `Delivery::key() -> QueryKey`.** `TileContent::deliver` takes it,
   and the six occupants that implement it (blotter, diagnostics,
   placeholder, recording, the shell-test-only `WatchingContent`, and
   this Part's own `MarketDataContent`, `geode-marketdata/src/content.rs`
   — the panel Part 4 must wire `Upload` into) match it exhaustively
   with no wildcard arm, so Part 4's `Upload` variant forces a compile
   error at every site rather than a silent no-op.
5. **The fragment no-shadow rule is enforced textually, on the
   predicate's tokens, not by evaluating the compiled boolean tree.**
   `check_fragment` refuses a predicate containing `!`, `||` or `(`
   anywhere in it, not only as a leading token: `blotter || workspace`
   and `(!blotter)` both pass a naive "starts with my context" scan
   while binding outside the module's own tile, which a tree
   evaluation would need a probe stack to catch and a token scan does
   not. `!=` is refused for the same reason (it contains `!`); neither
   shipped fragment needs it.
6. **`splice` partitions the layered docs on `Layer::Builtin`, rather
   than splicing fragments at a fixed index** — future-proofing, not a
   live case: exactly one builtin keymap doc exists today
   (`defaults::BUILTIN_KEYMAP`; `--demo`'s generated desk layer ships
   no `keymap` doc). Should a binary ever compile in a second builtin
   keymap doc, a fixed insertion point would misorder it relative to
   the fragments, where the partition keeps every builtin doc ahead of
   them by construction.
7. **A fragment's diagnostics are carried on
   `ShellServices.keymap_fragment_diagnostics`, separately from
   `ShellServices.keymap_diagnostics`, and folded into the config
   section by `apply_reload` *after* `reload::decide`** — not merged
   into the config doc a reload validates against. By the time a
   reload runs, `check_fragment` has already dropped the offending
   binding, so `build_keymap` has nothing left to report; without the
   separate carry, a fragment's diagnostic vanished from the
   diagnostics tile on the session's first hot reload.
8. **The reserved-action tables' retirement is total.**
   `BLOTTER_ACTION_DEFS`/`BLOTTER_ACTIONS`/`DIAGNOSTICS_ACTION_DEFS`/
   `DIAGNOSTICS_ACTIONS`, their registration loops and both mirror
   tests are deleted from `geode-shell`; the 56 moved bindings live
   verbatim in `geode_blotter::content::DEFAULT_KEYMAP` and
   `geode_diagnostics::DEFAULT_KEYMAP`, each crate now asserting its
   own fragment binds exactly its own registered actions, in both
   directions.
9. **`format_number`/`Formatted`/`Sign` moved from `geode-blotter` to
   `geode_core::format`**, re-exported at the old path so
   `geode-blotter`'s own tests (left in place) still cover the rule
   its cells depend on; `geode-core` carries no test of the formatter
   of its own, by design.
10. **`PanelSpec` gained `value_type: ColumnType`** (`F64` for `CVI`),
    not sketched in §8.1: a cell edit must parse through the column's
    *declared* type, and nothing the tile otherwise holds carries
    one — `Snapshot`'s `ColumnMeta` is name, attribution and scope
    semantics, and reading the arrow array's runtime kind would let
    an `i64` column whose values all happen to fit an `f64` array
    silently accept `0.5`.
11. **`MatrixModel::build` refuses more than §8.2 states**: a hole
    naming the row/column pair, a repeated pivot pair, a repeated flat
    row label, a NULL axis or key cell, and — added in review — more
    than one value column under `Columns::Axis`. A zero-row document
    is *not* an error; it builds an empty model the tile paints as "no
    document received."
12. **`Draft`'s cross-generation identity is `(row label, column
    label)`, resolved by two separate `HashMap<&str, usize>` lookups
    (`O(R + C)`), not an `R×C` index.** The first cut built the full
    cross product (50,000 entries at 10,000×5 to resolve at most a few
    hundred edits); `Draft::rebase` over 1,000 edits fell from 1.15 ms
    to 297 µs once split. Both lookups are correct only because a
    `MatrixModel`'s labels are unique on both axes by construction
    (the refusal list in 11 above) — `rebase` has no collision check
    of its own and must not grow one, or it would hide `build`'s own
    check from the mutation harness.
13. **Benched** (`cargo bench -p geode-marketdata`, p50): `MatrixModel::build`
    285 µs at CVI's 20×30 pivot, 8.18 ms at a 10,000×5 flat schedule
    fixture; `Draft::rebase` over 1,000 edits 297 µs. The flat build
    sits at the edge of the §7.1 8 ms pure-UI budget and is paid again
    on the keystroke that commits an edit or runs `:bump` (both call
    `rebuild_model`) — a schedule-shaped panel (roadmap slice 2) must
    patch the touched cells in place rather than call `build`
    wholesale; CVI itself, the only panel that exists, is unaffected
    at 285 µs. See `docs/perf.md`'s "Market-data panel" section.
14. **`MarketDataTile` answers the flip barrier honestly, in the
    blotter's own shape**, which §8.2/§8.6 did not specify:
    `self_arrive` on a change it does not requery for, staging a
    delivery under an open barrier and promoting on the flip bump
    whenever the staged versions still agree with the frame on every
    counter the tile FOLLOWS (§8.7.24),
    arriving on an ordinary delivery, a stale tag or an `Err`, and
    arriving with `acted` cleared (forcing a retry) on a refused
    submit. **The diagnostics tile had the identical gap since Phase
    4b** — as a non-placeholder occupant (`visible_tile_keys` filters
    only on `kind == "placeholder"`) that never calls
    `Frame::arrived`, one open diagnostics tile held every blotter to
    the 250 ms `FLIP_DEADLINE` on every scope, grouping or as-of
    change. This is a pre-existing defect, fixed on this branch — not
    new panel behaviour.
15. **`set_key` drops a staged snapshot and `set_visible(false)`
    clears `acted`**, neither stated in §8.3/§8.5: a key change bumps
    no frame version, so a snapshot staged for the old key would
    otherwise still pass the flip-identity check and paint one
    document's grid under another's header; a request cancelled by
    hiding the tile must not be remembered as "already asked," or a
    reshow paints a stale generation until the next unrelated publish.
16. **`:key` refuses outright while the draft has edits** — a ruling
    closing an ambiguity §8.3 leaves unstated, rather than the
    alternative of discarding the draft and reporting it, which would
    misfile a trader's numbers under a document they never chose to
    move to. The `:` line unconditionally re-requests the catalog on
    every `key` line rather than gating on staleness, because a
    subscribed feed can grow the key list within one completion
    session — `request_catalog()` plus `cx.notify()` in the same
    update, inside `command` rather than `completions` (which takes
    only `&App` and can queue nothing), is the trap a maintainer must
    keep together, since the request is otherwise queued and invisible
    until some unrelated mutation happens to notify.
17. **Session restore rebases against the first NON-EMPTY,
    successfully built model, not a config-time one.** `from_toml`
    parks each restored edit at the unaddressable column `usize::MAX`
    (`UNRESOLVED_COLUMN`) because the session stores label pairs, not
    grid indices, and no model exists until the first delivery; the
    tile calls `rebase` there unless the restored `base` already
    differs from the delivered one, in which case the draft lands
    directly in `Behind` with the newer document painted and the edits
    parked. **An empty snapshot and a refused build both leave the
    draft parked for the next delivery** (§8.7.24): rebasing against
    an empty grid dropped every restored edit silently, and §8.5 says
    unsent edits survive a restart. While parked the header reads
    `N restored edits awaiting a document`, and a label pair the
    delivered document cannot place is named in the notice exactly as
    `:rebase` names one.
18. **A commit whose cell moved out from under it is refused**
    (`CELL_MOVED`), which §8.3/§8.4 do not mention: `Editing` captures
    the cell and its labels at open, and a delivery landing mid-edit
    (which can shorten the document and reflow the grid) is compared
    against them at commit time rather than cancelling the editor on
    delivery — the latter would need a `Window` the frame observer
    does not have.
19. **`:revert` while `Behind` goes through the same `leave_behind`
    door `:discard` uses**, added in review: the first cut called
    `Draft::revert` directly, leaving `base_snapshot` retained with an
    empty, `Clean` draft and nothing left on screen to explain why the
    base generation was still painted, and `:rebase`/`:discard` (both
    gated on `is_behind()`) refused with nothing to move or drop.
20. **"CVI: Split" is offered whenever a data bridge exists, ungated on
    the `cvi_params` dataset being declared** — the palette row exists
    unconditionally, exactly as the blotter's does, and a panel opened
    with no such dataset shows its own notice rather than the palette
    hiding the option.
21. **Display checks are pending on a real window**, as every recent
    branch's are: the panel's paint at 10,000 rows, the editor `Input`
    painted in the cursor cell, the diagnostics tile's rows for a
    subscribed source, and every claim about theme colours in the
    panel body are all unverified pixel-for-pixel — verified instead
    against window-test assertions and by reading.
22. **`ModuleFactory::contexts()` is a separate answer from `kind()`**
    because the two names are genuinely independent: the market-data
    panel factory is kind `cvi` (one roster entry per document kind)
    and returns `vec!["marketdata"]` from `contexts()` (one vocabulary
    shared by every document kind's panel). The trait's default body
    (`vec![self.kind()]`) is what every module whose context and kind
    match wants, but taking it here would leave this panel with no
    keys at all, since `check_fragment` drops a binding whose first
    identifier names a context the factory did not declare.
23. **The keybindings dialog reads a fragment binding as
    `Layer::Builtin`**, the same layer the shell's own shipped keymap
    reports: `r` removes a user override and lets the fragment show
    through again, and `d` writes a user-layer `"none"` shadow over it
    rather than trying to remove a binding the trader's own config
    does not own —
    `d_over_a_modules_fragment_binding_writes_a_user_layer_shadow` and
    `r_removes_a_user_override_and_the_modules_fragment_shows_through`
    (`shell/tests/keybindings_dialog.rs`).
24. **The final whole-branch review's fix wave (2026-09-14)** — nine
    findings, and the five that changed behaviour a maintainer must
    know about:
    - **A staged snapshot promotes when it still answers what the tile
      FOLLOWS**, never on the barrier's own flip identity
      (`differs_on_followed`, shared with `follows_changed` at both
      sites so the two cannot drift). `FlipBarrier`'s doc says a later
      mutation "simply replaces it outright", and a replacement over a
      counter the tile does not follow comes with no requery at all —
      so the identity check threw away the only answer that tile would
      ever get, leaving the pre-mutation generation painted with
      `acted` claiming it was current. The panel follows `as_of`/`data`
      (a scope keystroke or a grouping step is the everyday case); the
      blotter has the same hole for an `unscoped` or pinned tile, fixed
      under the mechanism rule. The gate still drops a stage whose own
      followed counters moved — reachable while HIDDEN, where no
      requery supersedes it.
    - **A restored draft resolves only against a model that can resolve
      it** (§8.7.17 above), is reported rather than pruned in silence,
      and — since its base was never delivered — never lets a later
      delivery pin the painted generation as that base: a generation is
      retained under `Behind` only when the outgoing snapshot's own
      source time IS `draft.base`.
    - **A delivery `MatrixModel::build` refuses changes nothing but the
      notice.** The model, `self.snapshot` and the draft's own state are
      decided on a copy and committed only after the build succeeds, so
      the panel cannot go `Behind` against a generation it never
      painted. One build per delivery still: with a base retained the
      screen keeps the model it already has (`on_delivered` moves only
      the draft's STATE, which `build` does not read), and the fresh
      build is a validation of the delivered generation.
    - **The delivery notice is cleared in `apply`** — on the delivery
      that PAINTS, ahead of every notice `apply` itself writes — rather
      than in `deliver`'s `Ok` arm, where a staged delivery wiped a
      `:rebase` report on any unrelated publish.
    - **Every keyboard verb that moves which TILE has focus re-arms
      `pending_focus_restore`** when window focus is on a non-shell
      handle (`ShellView::note_keyboard_focus_move`, called from
      `dispatch`'s workspace branch and `open_module`'s focus arm) —
      the keyboard sibling of the mouse rule every tile mouse-down
      already follows. `i` then `mod+l` otherwise left the cell editor
      focused and painted, so each following bare key was dispatched by
      the matcher AND typed into the abandoned cell. The editor
      persists until commit or cancel; `escape` on the tile once focus
      returns there still cancels it through the module fragment.
    Two recorded, without code: a `Draft`'s generation identity is its
    source time ALONE, so a republish that keeps its source time
    (`source_time = "document"` stamping a date's midnight, or a
    corrected file republish, which ties its predecessor's) is invisible
    to the draft and swaps the grid under index-keyed edits — harmless
    while the row/column set is unchanged (CVI's ladder is fixed per
    date), unreachable under `--demo` (`source_time = "receive"`), and a
    `gen_id`-in-provenance follow-up for Part 4; and the `Behind` chip
    says "different document received HH:MM", because an as-of step back
    delivers an OLDER generation into that state — with `:asof undo`
    redelivering the edits' own base, which returns the draft to
    `Editing` untouched.

### 8.8 As built (the table body, 2026-09-14)

The user's ruling — "visually I don't like how the CVI panel looks; we
should use gpui-component's datatable here too for visual unity" —
replaced §8.2's body. The chip header, the model, the draft, the keys,
the yank, the find, the `:` vocabulary and the flip-barrier behaviour
are all untouched; what changed is what paints the grid.

1. **`geode_marketdata::delegate::MatrixDelegate` is the whole of it**,
   and the crate still does not depend on `geode-blotter`: the shapes
   were copied, not the code. It holds the prepared `Rc<MatrixModel>`,
   a mirror of the tile's cursor and a mirror of the open cell editor,
   and nothing else. `columns_count` is `1 + model.columns.len()`,
   `rows_count` is `model.rows.len()`, and `column(ix)` answers the
   row-label column for `ix == 0` (the row axis's own name, left
   aligned, `ColumnFixed::Left`, not movable, not sortable) and one
   value column per model column (right aligned, `CELL_WIDTH`, not
   movable, not sortable). **Columns are not resizable either** (review
   Minor 2), on both `Column::resizable` and
   `TableState::col_resizable`: a panel has no presentation document to
   write a width into (`view_presentation.toml` belongs to a view), so a
   dragged width would live only in the delegate's `column()` answer —
   which `TableState::refresh` re-prepares `col_groups` from, and every
   model swap refreshes, so the drag would snap back on the next delivery
   (about every 5 s on the demo bus) or the next committed edit. A handle
   that undoes itself seconds later is worse than no handle; offer it
   again when a width has somewhere to be written.
2. **The tile's cursor stays the truth; the delegate mirrors it.**
   `MarketDataTile::sync_cursor` writes `cursor`/`editor` into the
   delegate and moves the table's own selection —
   `set_selected_col(MatrixDelegate::table_col(col))` then
   `set_selected_row(row)` then `scroll_to_row(row)`. The column is
   set BEFORE the row because each setter switches the component's
   selection mode and the row highlight paints only in row mode, so
   ending on the row is what makes the panel read like the blotter (a
   highlighted row plus a bordered cursor cell). The `+ 1` is the
   row-label column, which the cursor never enters: `h` at model
   column 0 stays put, and `MatrixDelegate::model_col` answers `None`
   for table column 0 rather than saturating to 0.
3. **Every model swap goes through `MarketDataTile::install_model`,
   which calls `TableState::refresh`.** The component caches each
   `column()`'s answer in `col_groups` at prepare time and paints its
   HEADER from that cache alone, so a delivery whose node ladder
   changed would keep the previous document's headers (and lay its
   cells out at the previous widths) without it — the same trap
   CLAUDE.md records for the blotter's line-number gutter. Both doors
   into a model change end there: `rebuild_model` (a draft change) and
   `apply`'s own assignment (a delivery).
4. **The mouse is the blotter's shape.** `cell_selectable(true)` with
   `row_header(false)` is what makes the component report WHICH COLUMN
   a click landed in (`TableEvent::SelectCell`), and it adds no row-number
   column of its own; `col_selectable(false)` and `sortable(false)`
   because a document's axes are the desk's own order. **A mouse click
   selects a cell and does nothing else**: the cursor moves there, a
   click on a row label moves the row and leaves the column alone, and a
   double-click does exactly what the single click already did (item 6
   is why). `TableEvent::DoubleClickedCell` is therefore not matched, and
   `SelectRow`/`SelectColumn` are not matched either — `sync_cursor`
   emits both, so matching them would re-enter the handler on every
   cursor move. **A click while the cell editor is open cancels it**
   before the cursor moves (review Minor 5) — through `close_editor`, so
   blur then drop, and never a commit: a click is not `enter`, and
   writing a half-typed number because the trader clicked elsewhere is
   the one outcome nobody asked for. Cancelling is not optional, since
   the same mouse-down has already re-armed the shell's focus restore:
   left open, the editor would sit painted on the cell the cursor just
   left, deaf to the keyboard, with `mode == insert` still claimed and
   `enter` still bound to commit it. That cancel is the only reason the
   subscription is `cx.subscribe_in` and takes a `Window` at all.
5. **`geode_marketdata::init` binds the `DataTable` context's keys to
   `NoAction`, exactly as `geode_blotter::init` does**, and `main.rs`
   calls it beside the blotter's. A second copy rather than a shared
   function: this crate must not depend on `geode-blotter`, and binding
   the same keys twice is harmless. Focus is otherwise unchanged — the
   table is never focused, `close_editor` still blurs then drops, and
   `key_context` still reports `insert` exactly while `editor` is
   `Some`.
6. **Editing is keyboard-only, by controller ruling 2026-09-14: the
   mouse selects a cell, `i`/`enter` edit it.** A double-click was
   briefly wired to `marketdata::edit` and was withdrawn before review,
   because the editor it opened could not keep the keyboard: every tile
   mouse-down re-arms the shell's `pending_focus_restore` (CLAUDE.md's
   focus rule), which the next `ShellView::render` consumes by focusing
   the shell root, so the `Input` `begin_edit` had just focused went
   deaf on the following frame — the state `geode-shell`'s own
   `the_insert_branch_needs_the_tile_to_hold_focus_not_just_insert_mode`
   pins as legitimate (the editor stays open, the matcher governs the
   keyboard, `escape` cancels). A panel does not offer an affordance it
   cannot honour, and documenting one as broken is worse than not
   shipping it. Whether a tile occupant may deliberately hold focus
   through that restore is a shell-side focus decision, **deferred** —
   not a panel limitation to work around from inside a module. Should it
   be made, reinstating the mapping is one more arm in the `TableEvent`
   subscription (`DoubleClickedCell` -> `begin_edit`, which needs
   `cx.subscribe_in` for its `Window`), and
   `a_double_click_only_moves_the_cursor` is the test that would have to
   change with it. A click while an editor IS open cancels it (item 4),
   which is the same ruling read from the other side: the mouse may end
   an edit, never start one.
7. **Display checks are pending on a real window**, as §8.7.21's are —
   and this change adds to that list rather than clearing any of it:
   the table body's own chrome next to a blotter's, the cursor cell's
   border, the edited and sent cell styles, the pinned row-label
   column under horizontal scroll, and the editor `Input` inside a
   table cell.

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
panel's `MatrixModel` build and its paint at 20×30 and at 10,000 rows
× 5 columns (a broad-index dividend schedule), which the table must
hold under the 8 ms pure-UI budget because only the visible rows are
laid out (superseded 2026-09-14: "the uniform list" was the body when
this was written); the scenario panel's T×S grid (slice 2) inherits the
same table.

## 12. Sequencing

Four mergeable parts, each reviewed and merged before the next starts.

1. **Data model and request** — §3, §4, §7, all headless: family,
   validation, `publish_document`, the not-applicable marker, the
   document request, the catalog listing keys. Mutation entries land
   with each behaviour. **Done, 2026-09-13 (§4.5).**
2. **Adapter tier and documents** — §5, §6, §10's generator: traits,
   registry, channel adapter, coalescer, receiver pipeline, the
   `geode-documents` crate with CVI, the demo generator, registration
   in the app. `--demo` publishes documents into the database with no
   panel yet; the diagnostics tile's data section shows them. **Done,
   2026-09-13 (§5.6).**
3. **Panel and shell changes** — §8: the crate, the spec, the matrix
   model, keys, the draft with `Behind`/`rebase`/`discard`, session,
   `Delivery`, keymap fragments, the reserved-list deletion. **Done,
   2026-09-14 (§8.7).**
4. **Egress** — §9: config, request, outcome, confirm, `Sent` and the
   echo. **Remaining.**

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
